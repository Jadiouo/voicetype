#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
    worker::{DesktopSnapshot, DesktopWorker, RecoveryText},
    Provider,
};

struct DesktopState {
    worker: DesktopWorker,
    models: ModelSetup,
    tray_available: AtomicBool,
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
                worker: DesktopWorker::spawn(config_dir)?,
                models: ModelSetup::new(model_root)?,
                tray_available: AtomicBool::new(false),
            });
            let show = MenuItem::with_id(app, "show", "開啟 VoiceType", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "結束預覽 App", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let mut tray = TrayIconBuilder::with_id("main-tray")
                .tooltip("VoiceType Preview · 設定")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_settings(app),
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
            cancel_model_setup
        ])
        .build(tauri::generate_context!())
        .expect("VoiceType could not start")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
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
