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

#[cfg(target_os = "linux")]
use voicetype_app_core::google::{
    adapter::GoogleCliBoundary,
    legacy_systemd::{fcitx_identity, SystemdLegacyService},
    legacy_transfer::{GoogleShortcutTransfer, LegacyService, TransferPhase},
    linux_audio::LinuxPcmAudio,
    release::{inspect_linux_install, GoogleCliRelease},
    terminal::TerminalLaunch,
    GoogleInstallState, GoogleSetup, GoogleText,
};

#[cfg(target_os = "linux")]
fn google_transfer_resources(
    app: &tauri::AppHandle,
) -> Result<(GoogleShortcutTransfer, SystemdLegacyService), String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("找不到登入者的主目錄")?);
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let runtime = PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").ok_or("無法找到登入環境，請從桌面重新開啟 App")?,
    );
    if !config_home.is_absolute() || !runtime.is_absolute() {
        return Err("登入環境路徑無效".into());
    }
    let transfer = GoogleShortcutTransfer::new(
        app.state::<DesktopState>().config_dir.clone(),
        config_home.join("fcitx5/conf/googlevoiceinput.conf"),
        runtime.join("googlevoiceinput/bridge.lock"),
    )
    .map_err(|e| e.to_string())?;
    let service = SystemdLegacyService::new(&runtime).map_err(|e| e.to_string())?;
    Ok((transfer, service))
}

struct DesktopState {
    worker: DesktopWorker,
    models: ModelSetup,
    tray_available: AtomicBool,
    #[cfg(any(target_os = "linux", windows))]
    local_installer: voicetype_app_core::local_install::LocalInstaller,
    #[cfg(any(target_os = "linux", windows))]
    runtime_resources: PathBuf,
    #[cfg(any(target_os = "linux", windows))]
    local_asset_lock: std::sync::Mutex<()>,
    config_dir: PathBuf,
    legacy_vocabulary: PathBuf,
    review_monitor: std::sync::Mutex<Option<review::Monitor>>,
    spelling_installer: voicetype_app_core::spelling_install::SpellingInstaller,
    spelling_resources: PathBuf,
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
    #[cfg(any(target_os = "linux", windows))]
    {
        with_desktop(app.clone(), move |worker| {
            let state = app.state::<DesktopState>();
            let _assets = state
                .local_asset_lock
                .lock()
                .map_err(|_| "安裝程序已中斷")?;
            local_assets_idle(worker)?;
            // Hash/copy work stays off the resident dictation thread. No paths,
            // URLs, manifests or command lines are accepted from the webview.
            let mut paths = state
                .local_installer
                .prepare(&state.runtime_resources, |_, _| true)
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => {
                        "請先下載／檢查模型，再載入本機引擎。".to_string()
                    }
                    _ => "無法核對本機引擎或模型，請重新檢查模型與安裝包。".to_string(),
                })?;
            if worker.settings()?.settings.spelling_enabled {
                // Correction is optional. The worker exposes unavailable when
                // package verification fails while preserving the preference.
                paths.spelling = state
                    .spelling_installer
                    .prepare(&state.spelling_resources)
                    .ok();
            }
            #[cfg(target_os = "linux")]
            {
                worker.activate_local(paths)
            }
            #[cfg(windows)]
            {
                worker.activate_windows_local(paths)
            }
        })
        .await
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = app;
        Err("此平台的本機引擎封裝仍在準備中。".into())
    }
}

#[cfg(any(target_os = "linux", windows))]
fn local_assets_idle(worker: &DesktopWorker) -> Result<(), String> {
    use voicetype_app_core::worker::{GoogleRuntimeStatus, LocalRuntimeStatus};
    let snapshot = worker.settings()?;
    if snapshot.settings.dictation.busy
        || snapshot.local_runtime != LocalRuntimeStatus::Inactive
        || snapshot.google_runtime != GoogleRuntimeStatus::Inactive
        || snapshot.input_requested
    {
        return Err("請先結束聽寫並卸載 App 引擎，再變更本機引擎檔案".into());
    }
    Ok(())
}

#[tauri::command]
async fn repair_local_runtime(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(any(target_os = "linux", windows))]
    {
        return with_desktop(app.clone(), move |worker| {
            let state = app.state::<DesktopState>();
            let _assets = state
                .local_asset_lock
                .lock()
                .map_err(|_| "安裝程序已中斷")?;
            local_assets_idle(worker)?;
            state
                .local_installer
                .repair(&state.runtime_resources, |_, _| true)
                .map_err(|_| "無法修復本機引擎：請檢查模型與安裝包，原有資料已保留".to_string())?;
            worker.settings()
        })
        .await;
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = app;
        Err("此平台尚無本機引擎修復".into())
    }
}

#[tauri::command]
async fn rollback_local_runtime(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(any(target_os = "linux", windows))]
    {
        return with_desktop(app.clone(), move |worker| {
            let state = app.state::<DesktopState>();
            let _assets = state
                .local_asset_lock
                .lock()
                .map_err(|_| "安裝程序已中斷")?;
            local_assets_idle(worker)?;
            state
                .local_installer
                .rollback(|_, _| true)
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => {
                        "沒有可回復的上一版本機引擎，或模型尚未準備完成".to_string()
                    }
                    _ => "上一版引擎未通過完整核對，或回復狀態無法確認；請重新開啟 App 檢查"
                        .to_string(),
                })?;
            worker.settings()
        })
        .await;
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = app;
        Err("此平台尚無本機引擎回復".into())
    }
}

#[tauri::command]
async fn unload_local_runtime(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, DesktopWorker::deactivate_local).await
}

#[cfg(target_os = "linux")]
fn official_google_path() -> Result<PathBuf, String> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("找不到登入者的主目錄")?);
    if !home.is_absolute() {
        return Err("登入者的主目錄無效".into());
    }
    Ok(home.join(".local/bin/agy"))
}

#[cfg(target_os = "linux")]
fn bundled_google_helper(app: &tauri::AppHandle, name: &str) -> Result<(PathBuf, String), String> {
    let catalog: voicetype_app_core::assets::AssetManifest = serde_json::from_str(include_str!(
        concat!(env!("OUT_DIR"), "/google-catalog.json")
    ))
    .map_err(|_| "Google 輔助程式目錄無效")?;
    if catalog.id != "google-helpers" || catalog.platform != "linux-x86_64" {
        return Err("Google 輔助程式平台不相容".into());
    }
    let item = catalog
        .files
        .iter()
        .find(|item| item.path == name && item.executable)
        .ok_or("Google 輔助程式未封裝")?;
    let path = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("google")
        .join(name);
    if GoogleSetup::new(path.clone(), item.sha256.clone()).inspect()
        != GoogleInstallState::OfficialCheckRequired
    {
        return Err("Google 輔助程式未通過安裝包核對".into());
    }
    Ok((path, item.sha256.clone()))
}

#[tauri::command]
async fn load_google_runtime(app: tauri::AppHandle) -> Result<View, String> {
    load_google_runtime_mode(app, false).await
}

async fn load_google_runtime_mode(
    app: tauri::AppHandle,
    transfer_mode: bool,
) -> Result<View, String> {
    #[cfg(target_os = "linux")]
    {
        let official = official_google_path()?;
        let install = inspect_linux_install(&official);
        if install.state != GoogleInstallState::OfficialCheckRequired {
            return Err(
                "請先依官方說明安裝相容的 Antigravity CLI；目前版本未通過核對，未開啟麥克風".into(),
            );
        }
        let home = PathBuf::from(std::env::var_os("HOME").ok_or("找不到登入者的主目錄")?);
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        if !config_home.is_absolute() {
            return Err("輸入法設定位置無效".into());
        }
        let legacy = config_home.join("fcitx5/conf/googlevoiceinput.conf");
        if !transfer_mode
            && voicetype_app_core::google::legacy_config::ctrl_caps_conflict(&legacy)
                .map_err(|_| "無法核對現有 Google 快捷鍵設定；未接管錄音".to_string())?
        {
            return Err("現有 Google 輸入法也使用 Ctrl＋CapsLock；為避免同時錄音，App 目前未接管。請保留現有設定，等待可回復的遷移操作。".into());
        }
        let release = match install.version {
            Some("1.2.12") => GoogleCliRelease::Linux1212,
            Some("1.3.1") => GoogleCliRelease::Linux131,
            _ => return Err("Google CLI 版本尚未驗證".into()),
        };
        let (editor, _) = bundled_google_helper(&app, "voicetype-google-editor")?;
        let (pcm, _) = bundled_google_helper(&app, "voicetype-google-pcm")?;
        let state = app.state::<DesktopState>();
        let config_dir = state.config_dir.clone();
        let spelling = if state.worker.settings()?.settings.spelling_enabled {
            state
                .spelling_installer
                .prepare(&state.spelling_resources)
                .ok()
                .and_then(|paths| paths.command().ok())
                .and_then(|command| {
                    voicetype_text::spelling::OwnedSpelling::start(
                        command,
                        std::time::Duration::from_secs(30),
                    )
                    .ok()
                })
        } else {
            None
        };
        let spelling_ready = spelling.is_some();
        let review = voicetype_app_core::review_collector::Collector::with_paths(
            config_dir.join("review.json"),
            config_dir.join("review"),
        );
        let runtime = PathBuf::from(
            std::env::var_os("XDG_RUNTIME_DIR").ok_or("無法找到登入環境，請從桌面重新開啟 App")?,
        );
        // A staged handoff is never an ordinary Google load. The worker route
        // must stay Start-gated until it owns the old bridge's lifetime lock.
        if !transfer_mode {
            match std::fs::symlink_metadata(config_dir.join("google-shortcut-rollback.json")) {
                Ok(_) => {
                    return Err("Google 快捷鍵交接尚未完成；請使用明確的切換或還原操作。".into())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("無法核對 Google 快捷鍵交接紀錄；錄音未開始。".into()),
            }
            match std::fs::symlink_metadata(runtime.join("googlevoiceinput/bridge.lock")) {
                Ok(_) => {
                    return Err("偵測到舊 Google bridge 鎖；請使用明確的可回復切換操作。".into())
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("無法核對舊 Google bridge；錄音未開始。".into()),
            }
            let legacy_service =
                voicetype_app_core::google::legacy_systemd::SystemdLegacyService::new(&runtime)
                    .map_err(|_| "無法核對舊 Google 服務；錄音未開始。".to_string())?;
            match voicetype_app_core::google::legacy_transfer::LegacyService::snapshot(
                &legacy_service,
            ) {
                Ok(unit) if unit.active => {
                    return Err("舊 Google 服務仍在執行；請先使用明確的可回復切換操作。".into())
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("無法核對舊 Google 服務；錄音未開始。".into()),
            }
        }
        let launch = TerminalLaunch {
            executable: official,
            sha256: release.reviewed_sha256().into(),
            workspace: config_dir.clone(),
            profile: runtime.clone(),
            editor,
        };
        let vocab = config_dir.join("vocab.toml");
        with_desktop(app, move |worker| {
            let profile = runtime.clone();
            let create = move || {
                let audio = LinuxPcmAudio::new(
                    &profile,
                    std::path::Path::new("/usr/bin/pw-record"),
                    "3874ae059c9eafdbd4d6fde4d4b7553aa83fe4149246e427a3208bdc4c712c64",
                    &pcm,
                )?
                .with_review_config(config_dir.join("review.json"));
                GoogleCliBoundary::open(launch.clone(), audio, std::time::Duration::from_secs(18))
            };
            let format = move |raw: String| {
                GoogleText::open(&vocab)?.apply(&raw, |text, terms| {
                    spelling
                        .as_ref()
                        .map_or_else(|| text.to_owned(), |worker| worker.correct(text, terms))
                })
            };
            if transfer_mode {
                worker.prepare_google_transfer(runtime, create, format, spelling_ready, review)
            } else {
                worker.activate_google(runtime, create, format, spelling_ready, review)
            }
        })
        .await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("此平台的 Google 錄音完整性尚未通過驗證。".into())
    }
}

#[tauri::command]
async fn google_shortcut_phase(app: tauri::AppHandle) -> Result<Option<String>, String> {
    #[cfg(target_os = "linux")]
    {
        let (transfer, _) = google_transfer_resources(&app)?;
        tauri::async_runtime::spawn_blocking(move || {
            transfer
                .phase()
                .map(|phase| {
                    phase.map(|phase| match phase {
                        TransferPhase::DisableStaged => "disable_staged".to_owned(),
                        TransferPhase::AppOwned => "app_owned".to_owned(),
                        TransferPhase::RestoreStaged => "restore_staged".to_owned(),
                    })
                })
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|_| "Google 交接狀態查詢中斷".to_string())?
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Ok(None)
    }
}

/// The UI action only stages disabled bytes; it does not stop the old unit.
/// Fcitx must load the staged bytes under a new process identity before claim.
#[tauri::command]
async fn stage_google_shortcut(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(target_os = "linux")]
    {
        let official = official_google_path()?;
        let install =
            tauri::async_runtime::spawn_blocking(move || inspect_linux_install(&official))
                .await
                .map_err(|_| "Google CLI 核對中斷".to_string())?;
        if install.state != GoogleInstallState::OfficialCheckRequired {
            return Err("請先安裝並核對官方 Google CLI，再準備切換舊服務".into());
        }
        with_worker(app.clone(), DesktopWorker::begin_google_transfer_setup).await?;
        let (transfer, service) = google_transfer_resources(&app)?;
        let staged = tauri::async_runtime::spawn_blocking(move || {
            let fcitx = fcitx_identity().map_err(|e| e.to_string())?;
            let unit = service.snapshot().map_err(|e| e.to_string())?;
            transfer
                .stage_disable(fcitx, unit)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|_| "Google 交接準備中斷".to_string())?;
        if let Err(error) = staged {
            let (transfer, _) = google_transfer_resources(&app)?;
            if matches!(transfer.phase(), Ok(None)) {
                let _ = with_worker(app.clone(), move |worker| {
                    worker.finish_google_transfer_setup(transfer)
                })
                .await;
            }
            return Err(error);
        }
        with_desktop(app, DesktopWorker::settings).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("此平台不使用 Fcitx Google 快捷鍵交接。".into())
    }
}

/// This is the second, explicit setup action after Fcitx has loaded the
/// disabled shortcut. A v2 peer PID, service quiescence and the old bridge
/// lock must all agree before the resident Start gate opens.
#[tauri::command]
async fn switch_google_shortcut(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(target_os = "linux")]
    {
        let (transfer, service) = google_transfer_resources(&app)?;
        if transfer.phase().map_err(|e| e.to_string())? != Some(TransferPhase::DisableStaged) {
            return Err("請先準備 Google 快捷鍵切換".into());
        }
        if with_worker(app.clone(), DesktopWorker::google_endpoint)
            .await?
            .is_none()
        {
            load_google_runtime_mode(app.clone(), true).await?;
        }
        let peer = with_worker(app.clone(), |worker| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
            loop {
                if let Some(pid) = worker.google_transfer_route_pid()? {
                    return Ok(pid);
                }
                if std::time::Instant::now() >= deadline {
                    return Err("等待新版 Fcitx v2 路由逾時；舊服務尚未停止，錄音仍鎖定".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
        .await?;
        let (owned, identity) = tauri::async_runtime::spawn_blocking(move || {
            let identity = fcitx_identity().map_err(|e| e.to_string())?;
            if identity.pid != peer {
                return Err("Fcitx 路由身分已變更；舊服務尚未停止".into());
            }
            let owned = transfer
                .claim(identity.clone(), true, &service)
                .map_err(|e| format!("無法取得舊 Google 服務完整所有權：{e}"))?;
            Ok::<_, String>((owned, identity))
        })
        .await
        .map_err(|_| "Google 交接程序中斷；錄音仍鎖定".to_string())??;
        // A process replacement during stop/lock acquisition cannot inherit
        // this claim. Park the lock in the resident worker for explicit restore.
        let still_same = tauri::async_runtime::spawn_blocking(fcitx_identity)
            .await
            .map_err(|_| "無法再次核對 Fcitx 身分".to_string())?
            .map(|now| now == identity)
            .unwrap_or(false);
        if !still_same {
            with_desktop(app, move |worker| {
                worker.park_google_transfer_ownership(owned)
            })
            .await?;
            return Err("Fcitx 身分在交接期間改變；服務鎖由 App 保留，請還原後重試".into());
        }
        with_desktop(app, move |worker| {
            worker.complete_google_transfer(owned, identity)
        })
        .await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("此平台不使用 Fcitx Google 快捷鍵交接。".into())
    }
}

#[tauri::command]
async fn restore_google_shortcut(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(target_os = "linux")]
    {
        let (transfer, service) = google_transfer_resources(&app)?;
        let phase = transfer.phase().map_err(|e| e.to_string())?;
        match phase {
            Some(TransferPhase::DisableStaged) => {
                with_worker(app.clone(), DesktopWorker::deactivate_google).await?;
                tauri::async_runtime::spawn_blocking(move || {
                    transfer
                        .abort_staged(fcitx_identity().map_err(|e| e.to_string())?, true, &service)
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|_| "Google 還原程序中斷".to_string())??;
                with_desktop(app, DesktopWorker::settings).await
            }
            Some(TransferPhase::AppOwned) => {
                let owned = with_worker(app.clone(), DesktopWorker::google_transfer_owned).await?;
                if !owned {
                    let ownership = tauri::async_runtime::spawn_blocking(move || {
                        transfer
                            .reclaim_for_restore(&service)
                            .map_err(|e| e.to_string())
                    })
                    .await
                    .map_err(|_| "無法恢復舊 Google 服務鎖".to_string())??;
                    with_worker(app.clone(), move |worker| {
                        worker.park_google_transfer_ownership(ownership)
                    })
                    .await?;
                } else if with_worker(app.clone(), DesktopWorker::google_endpoint)
                    .await?
                    .is_some()
                {
                    with_worker(app.clone(), DesktopWorker::begin_google_transfer_restore).await?;
                }
                with_worker(app.clone(), DesktopWorker::deactivate_google).await?;
                let (transfer, service) = google_transfer_resources(&app)?;
                with_desktop(app, move |worker| {
                    worker.restore_google_transfer(transfer, service)
                })
                .await
            }
            Some(TransferPhase::RestoreStaged) => {
                with_worker(app.clone(), DesktopWorker::deactivate_google).await?;
                tauri::async_runtime::spawn_blocking(move || {
                    transfer
                        .resume_restore(true, &service)
                        .map_err(|e| e.to_string())?;
                    transfer
                        .finish_restore(fcitx_identity().map_err(|e| e.to_string())?, &service)
                        .map_err(|e| format!("請重新載入 Fcitx 後再按一次「完成還原」：{e}"))
                })
                .await
                .map_err(|_| "Google 還原程序中斷".to_string())??;
                let (transfer, _) = google_transfer_resources(&app)?;
                with_desktop(app, move |worker| {
                    worker.finish_google_transfer_setup(transfer)
                })
                .await
            }
            None => Err("目前沒有待還原的 Google 快捷鍵交接".into()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("此平台不使用 Fcitx Google 快捷鍵交接。".into())
    }
}

#[tauri::command]
async fn unload_google_runtime(app: tauri::AppHandle) -> Result<View, String> {
    #[cfg(target_os = "linux")]
    {
        with_desktop(app, DesktopWorker::deactivate_google).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("此平台的 Google 引擎尚未載入。".into())
    }
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
        #[cfg(windows)]
        {
            worker.enable_windows_input()
        }
        #[cfg(not(any(target_os = "linux", windows)))]
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
    windows_input: bool,
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
async fn set_spelling_enabled(enabled: bool, app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, move |worker| worker.set_spelling_enabled(enabled)).await
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
            let current = worker.settings()?;
            if current.settings.dictation.busy
                || current.google_runtime
                    == voicetype_app_core::worker::GoogleRuntimeStatus::Cleaning
            {
                return Err("錄音或清理進行中，請稍後重新檢查".into());
            }
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
            worker.refresh_local(socket)?;
            worker.refresh_google_install(official_google_path()?)
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
        local_runtime_bundled: cfg!(any(target_os = "linux", windows)),
        windows_input: cfg!(windows),
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
                spelling_installer: voicetype_app_core::spelling_install::SpellingInstaller::new(
                    config_dir.clone(),
                    serde_json::from_str(include_str!(concat!(
                        env!("OUT_DIR"),
                        "/spelling-catalog.json"
                    )))?,
                )?,
                spelling_resources: app.path().resource_dir()?.join("spelling"),
                #[cfg(any(target_os = "linux", windows))]
                local_installer: voicetype_app_core::local_install::LocalInstaller::bundled(
                    config_dir.clone(),
                    serde_json::from_str(include_str!(concat!(
                        env!("OUT_DIR"),
                        "/runtime-catalog.json"
                    )))?,
                )?,
                #[cfg(any(target_os = "linux", windows))]
                runtime_resources: app.path().resource_dir()?.join("runtime"),
                #[cfg(any(target_os = "linux", windows))]
                local_asset_lock: std::sync::Mutex::new(()),
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
            set_spelling_enabled,
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
            repair_local_runtime,
            rollback_local_runtime,
            unload_local_runtime,
            load_google_runtime,
            unload_google_runtime,
            google_shortcut_phase,
            stage_google_shortcut,
            switch_google_shortcut,
            restore_google_shortcut,
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
