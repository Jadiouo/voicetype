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
    Manager, State,
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
fn get_settings(state: State<'_, DesktopState>) -> Result<View, String> {
    let desktop = state
        .desktop
        .lock()
        .map_err(|_| "設定暫時無法讀取，請重新開啟 App")?;
    let settings = desktop
        .app
        .as_ref()
        .map_err(ToString::to_string)?
        .snapshot();
    Ok(View {
        settings,
        tray_available: state.tray_available.load(Ordering::Relaxed),
    })
}

#[tauri::command]
fn select_provider(provider: Provider, state: State<'_, DesktopState>) -> Result<View, String> {
    let mut desktop = state
        .desktop
        .lock()
        .map_err(|_| "設定暫時無法讀取，請重新開啟 App")?;
    let app = desktop.app.as_mut().map_err(|e| e.to_string())?;
    let settings = app.select_provider(provider).map_err(|e| e.to_string())?;
    Ok(View {
        settings,
        tray_available: state.tray_available.load(Ordering::Relaxed),
    })
}

#[tauri::command]
fn reload_settings(state: State<'_, DesktopState>) -> Result<View, String> {
    {
        let mut desktop = state
            .desktop
            .lock()
            .map_err(|_| "設定暫時無法讀取，請重新開啟 App")?;
        desktop.app = Application::open(&desktop.config_dir);
    }
    get_settings(state)
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
            let config_dir = app.path().app_config_dir()?;
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
            reload_settings
        ])
        .run(tauri::generate_context!())
        .expect("VoiceType could not start");
}
