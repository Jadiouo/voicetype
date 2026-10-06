#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};
use voicetype_app_core::{AppError, Application, Provider, Snapshot};

struct Desktop {
    config_dir: PathBuf,
    app: Result<Application, AppError>,
}

struct DesktopState {
    desktop: Mutex<Desktop>,
    tray_available: AtomicBool,
}

#[derive(serde::Serialize)]
struct View {
    settings: Snapshot,
    tray_available: bool,
}

#[tauri::command]
async fn get_settings(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, |desktop| {
        Ok(desktop.app.as_ref().map_err(|e| e.to_string())?.snapshot())
    })
    .await
}

#[tauri::command]
async fn select_provider(provider: Provider, app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, move |desktop| {
        desktop
            .app
            .as_mut()
            .map_err(|e| e.to_string())?
            .select_provider(provider)
            .map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
async fn reload_settings(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, |desktop| {
        if let Ok(app) = desktop.app.as_mut() {
            return app.reload().map_err(|e| e.to_string());
        }
        desktop.app = Application::open(&desktop.config_dir);
        Ok(desktop
            .app
            .as_ref()
            .map_err(ToString::to_string)?
            .snapshot())
    })
    .await
}

#[tauri::command]
async fn refresh_providers(app: tauri::AppHandle) -> Result<View, String> {
    with_desktop(app, |desktop| {
        let app = desktop.app.as_mut().map_err(|e| e.to_string())?;
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
            if !socket.is_absolute() {
                return Err("本機服務位置必須是完整路徑".into());
            }
            Ok(app.refresh_local_provider(&socket, std::time::Duration::from_millis(300)))
        }
        #[cfg(not(target_os = "linux"))]
        Ok(app.snapshot())
    })
    .await
}

// Settings IO and the bounded status probe never run on the webview event thread.
async fn with_desktop(
    app: tauri::AppHandle,
    operation: impl FnOnce(&mut Desktop) -> Result<Snapshot, String> + Send + 'static,
) -> Result<View, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<DesktopState>();
        let mut desktop = state
            .desktop
            .lock()
            .map_err(|_| "設定暫時無法讀取，請重新開啟 App")?;
        let settings = operation(&mut desktop)?;
        Ok(View {
            settings,
            tray_available: state.tray_available.load(Ordering::Relaxed),
        })
    })
    .await
    .map_err(|_| "設定工作中斷，請重新開啟 App".to_string())?
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
            app.manage(DesktopState {
                desktop: Mutex::new(Desktop {
                    app: Application::open(&config_dir),
                    config_dir,
                }),
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
            refresh_providers
        ])
        .run(tauri::generate_context!())
        .expect("VoiceType could not start");
}
