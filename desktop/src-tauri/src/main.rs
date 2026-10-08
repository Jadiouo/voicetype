#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod review;

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};
use voicetype_app_core::{
    setup::{ModelSetup, SetupStatus},
    vocabulary::{Vocabulary, VocabularyView},
    worker::{DesktopSnapshot, DesktopWorker, RecoveryText},
    Provider,
};

struct DesktopState {
    worker: DesktopWorker,
    models: ModelSetup,
    tray_available: AtomicBool,
    #[cfg(target_os = "linux")]
    local_installer: voicetype_app_core::local_install::LocalInstaller,
    #[cfg(target_os = "linux")]
    runtime_resources: PathBuf,
    config_dir: PathBuf,
    legacy_vocabulary: PathBuf,
    review_monitor: std::sync::Mutex<Option<review::Monitor>>,
}

// These commands run independently of the resident dictation queue. File
// validation/preview must never pause capture or delay a result already in flight.
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum VocabularyChange {
    Put {
        index: Option<usize>,
        wrong: Vec<String>,
        right: String,
    },
    Delete {
        index: usize,
    },
    Names {
        names: Vec<String>,
    },
    Terms {
        terms: Vec<String>,
    },
    Restore,
    Import,
}

#[tauri::command]
async fn get_vocabulary(app: tauri::AppHandle) -> Result<VocabularyView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Ok(Vocabulary::open(app.state::<DesktopState>().config_dir.join("vocab.toml"))?.snapshot())
    })
    .await
    .map_err(|_| "詞庫工作中斷".to_string())?
}

#[tauri::command]
async fn edit_vocabulary(
    revision: String,
    change: VocabularyChange,
    app: tauri::AppHandle,
) -> Result<VocabularyView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        let mut vocabulary = Vocabulary::open(state.config_dir.join("vocab.toml"))?;
        match change {
            VocabularyChange::Put {
                index,
                wrong,
                right,
            } => vocabulary.put(&revision, index, wrong, right),
            VocabularyChange::Delete { index } => vocabulary.delete(&revision, index),
            VocabularyChange::Names { names } => vocabulary.set_names(&revision, names),
            VocabularyChange::Terms { terms } => vocabulary.set_terms(&revision, terms),
            VocabularyChange::Restore => vocabulary.restore(&revision),
            VocabularyChange::Import => {
                vocabulary.import_existing(&revision, &state.legacy_vocabulary)
            }
        }
    })
    .await
    .map_err(|_| "詞庫工作中斷".to_string())?
}

#[tauri::command]
async fn preview_vocabulary(
    revision: String,
    text: String,
    app: tauri::AppHandle,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let vocabulary =
            Vocabulary::open(app.state::<DesktopState>().config_dir.join("vocab.toml"))?;
        if vocabulary.snapshot().revision != revision {
            return Err("詞庫已更新，請重新載入再預覽".into());
        }
        vocabulary.preview(&text)
    })
    .await
    .map_err(|_| "詞庫工作中斷".to_string())?
}

#[tauri::command]
async fn configure_input_module(restore: bool, app: tauri::AppHandle) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        let source = if restore {
            None
        } else {
            Some(
                app.path()
                    .resource_dir()
                    .map_err(|e| e.to_string())?
                    .join("input"),
            )
        };
        let registration = app
            .path()
            .data_dir()
            .map_err(|e| e.to_string())?
            .join("fcitx5/addon/voicetype.conf");
        let installer = voicetype_app_core::input_install::FcitxInstaller::new(
            app.state::<DesktopState>().config_dir.clone(),
            registration,
            serde_json::from_str(include_str!(concat!(
                env!("OUT_DIR"),
                "/input-catalog.json"
            )))
            .map_err(|_| "輸入法模組目錄無效")?,
        )
        .map_err(|_| "輸入法模組設定無效")?;
        let changed = with_worker(app, move |worker| {
            worker.configure_input_module(installer, source)
        })
        .await?;
        Ok(if !changed {
            "沒有需要還原的模組設定。"
        } else if restore {
            "原模組設定已還原。請在方便時重新登入桌面；執行中的輸入法未被重啟。"
        } else {
            "模組已安裝，原設定已備份。請先結束目前聽寫，再重新登入桌面，讓 Fcitx 載入新版。"
        }
        .into())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (restore, app);
        Err("此平台不使用 Fcitx 模組。".into())
    }
}

#[tauri::command]
async fn load_local_runtime(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(target_os = "linux")]
    {
        with_desktop(app.clone(), move |worker| {
            let state = app.state::<DesktopState>();
            // Hash/copy work stays off the resident dictation thread. No paths,
            // URLs, manifests or command lines are accepted from the webview.
            let paths = state
                .local_installer
                .prepare(&state.runtime_resources, |_, _| true)
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => {
                        "請先下載／檢查模型，再載入本機引擎。".to_string()
                    }
                    _ => "無法核對本機引擎或模型，請重新檢查模型與安裝包。".to_string(),
                })?;
            worker.activate_local(paths)
        })
        .await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("此平台的本機引擎封裝仍在準備中。".into())
    }
}

#[tauri::command]
async fn unload_local_runtime(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, DesktopWorker::deactivate_local).await
}

#[tauri::command]
async fn enable_local_input(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, |worker| {
        #[cfg(target_os = "linux")]
        {
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .ok_or("無法找到登入環境，請從桌面重新開啟 App")?;
            worker.enable_local_input(PathBuf::from(runtime))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = worker;
            Err("此平台的輸入整合仍在準備中。".into())
        }
    })
    .await
}

#[tauri::command]
fn get_model_setup(app: tauri::AppHandle) -> SetupStatus {
    app.state::<DesktopState>().models.status()
}

#[tauri::command]
async fn prepare_models(app: tauri::AppHandle) -> Result<SetupStatus, String> {
    // Only dispatch/join bookkeeping runs here; HTTP, hashing and extraction
    // stay on ModelSetup's separate thread. No webview paths or URLs accepted.
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<DesktopState>()
            .models
            .start()
            .map_err(|_| "無法開始模型準備，請稍後再試。".into())
    })
    .await
    .map_err(|_| "模型準備工作已中斷。".to_string())?
}

#[tauri::command]
fn cancel_model_setup(app: tauri::AppHandle) -> SetupStatus {
    app.state::<DesktopState>().models.cancel()
}

#[derive(serde::Serialize)]
struct View {
    #[serde(flatten)]
    desktop: DesktopSnapshot,
    tray_available: bool,
    local_runtime_bundled: bool,
}

#[tauri::command]
async fn get_settings(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, DesktopWorker::settings).await
}

#[tauri::command]
async fn select_provider(provider: Provider, app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, move |worker| worker.select_provider(provider)).await
}

#[tauri::command]
async fn reload_settings(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, DesktopWorker::reload).await
}

#[tauri::command]
async fn cancel_dictation(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, DesktopWorker::cancel_dictation).await
}

#[tauri::command]
async fn get_recovery(app: tauri::AppHandle) -> Result<Option<RecoveryText>, String> {
    with_worker(app, DesktopWorker::recovery).await
}

#[tauri::command]
async fn dismiss_recovery(
    provider: Provider,
    session: String,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    with_worker(app, move |worker| {
        worker.dismiss_recovery(provider, session)
    })
    .await
}

#[tauri::command]
async fn refresh_providers(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, |worker| {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let socket = match std::env::var_os("VOICETYPE_SOCKET") {
                Some(path) => PathBuf::from(path),
                None => match std::env::var_os("XDG_RUNTIME_DIR") {
                    Some(path) => PathBuf::from(path).join("voicetype/ipc.sock"),
                    None => {
                        let uid = std::fs::metadata("/proc/self")
                            .map_err(|e| e.to_string())?
                            .uid();
                        PathBuf::from(format!("/tmp/voicetype-{uid}/ipc.sock"))
                    }
                },
            };
            worker.refresh_local(socket)
        }
        #[cfg(not(target_os = "linux"))]
        worker.settings()
    })
    .await
}

// Only request/reply waiting uses the blocking pool. Runtime creation, events
// and cleanup all stay on DesktopWorker's resident thread.
async fn with_worker<T: Send + 'static>(
    app: tauri::AppHandle,
    operation: impl FnOnce(&DesktopWorker) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || operation(&app.state::<DesktopState>().worker))
        .await
        .map_err(|_| "App 工作中斷，請重新開啟".to_string())?
}

async fn with_desktop(
    app: tauri::AppHandle,
    operation: impl FnOnce(&DesktopWorker) -> Result<DesktopSnapshot, String> + Send + 'static,
) -> Result<View, String> {
    let tray_available = app
        .state::<DesktopState>()
        .tray_available
        .load(Ordering::Relaxed);
    Ok(View {
        desktop: with_worker(app, operation).await?,
        tray_available,
        local_runtime_bundled: cfg!(target_os = "linux"),
    })
}

fn show_settings(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_settings(app)
        }))
        .setup(|app| {
            // Distinct preview directory: never change the running dictation setup.
            let config_dir = match std::env::var_os("VOICETYPE_PREVIEW_CONFIG_DIR") {
                Some(path) => {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() {
                        return Err("VOICETYPE_PREVIEW_CONFIG_DIR must be absolute".into());
                    }
                    path
                }
                None => app.path().app_config_dir()?,
            };
            let model_root = config_dir.join("model-assets");
            app.manage(DesktopState {
                #[cfg(target_os = "linux")]
                local_installer: voicetype_app_core::local_install::LocalInstaller::bundled(
                    config_dir.clone(),
                    serde_json::from_str(include_str!(concat!(
                        env!("OUT_DIR"),
                        "/runtime-catalog.json"
                    )))?,
                )?,
                #[cfg(target_os = "linux")]
                runtime_resources: app.path().resource_dir()?.join("runtime"),
                review_monitor: std::sync::Mutex::new(None),
                config_dir: config_dir.clone(),
                legacy_vocabulary: std::env::var_os("VOICETYPE_VOCAB")
                    .map(PathBuf::from)
                    .unwrap_or(app.path().config_dir()?.join("voicetype/vocab.toml")),
                worker: DesktopWorker::spawn(config_dir)?,
                models: ModelSetup::new(model_root)?,
                tray_available: AtomicBool::new(false),
            });
            let show = MenuItem::with_id(app, "show", "開啟 VoiceType", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "結束預覽 App", true, None::<&str>)?;
            let reviews = MenuItem::with_id(app, "review", "抽樣校對", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &reviews, &quit])?;
            let mut tray = TrayIconBuilder::with_id("main-tray")
                .tooltip("VoiceType Preview · 設定")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_settings(app),
                    "review" => {
                        show_settings(app);
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.eval("window.dispatchEvent(new Event('review-open'))");
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            match tray.build(app) {
                Ok(_) => app
                    .state::<DesktopState>()
                    .tray_available
                    .store(true, Ordering::Relaxed),
                Err(error) => eprintln!("Tray unavailable: {error}"),
            }
            *app.state::<DesktopState>().review_monitor.lock().unwrap() =
                Some(review::monitor(app.handle().clone(), reviews)?);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            select_provider,
            reload_settings,
            refresh_providers,
            get_recovery,
            dismiss_recovery,
            cancel_dictation,
            get_model_setup,
            prepare_models,
            cancel_model_setup,
            load_local_runtime,
            unload_local_runtime,
            enable_local_input,
            configure_input_module,
            get_vocabulary,
            edit_vocabulary,
            preview_vocabulary,
            review::get_review,
            review::set_review_enabled,
            review::edit_review,
            review::get_review_audio
        ])
        .build(tauri::generate_context!())
        .expect("VoiceType could not start")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                app.state::<DesktopState>()
                    .review_monitor
                    .lock()
                    .unwrap()
                    .take();
                // Final exit event: let the resident owner reap before the
                // process ends. Parent-death cleanup also covers abrupt exits.
                if app.state::<DesktopState>().worker.shutdown().is_err() {
                    eprintln!("VoiceType worker could not finish clean shutdown");
                }
                if app.state::<DesktopState>().models.shutdown().is_err() {
                    eprintln!("VoiceType model setup could not finish clean shutdown");
                }
            }
        });
}
