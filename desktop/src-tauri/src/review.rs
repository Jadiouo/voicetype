//! Review IO runs off the dictation worker and accepts no webview file paths.
use crate::DesktopState;
use std::{
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{menu::MenuItem, Manager};
use voicetype_app_core::review::{ReviewItem, ReviewSettings, ReviewStore};

fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| "系統時間無效".into())
}
fn store(app: &tauri::AppHandle) -> Result<ReviewStore, String> {
    let state = app.state::<DesktopState>();
    ReviewStore::open(
        state.config_dir.join("review.json"),
        state.config_dir.join("review"),
    )
}
#[derive(serde::Serialize)]
pub struct ReviewView {
    settings: ReviewSettings,
    items: Vec<ReviewItem>,
}
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewChange {
    Save { corrected: String },
    Promote,
    Delete,
}

#[tauri::command]
pub async fn get_review(app: tauri::AppHandle) -> Result<ReviewView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = store(&app)?;
        Ok(ReviewView {
            settings: store.settings()?,
            items: store.list(now()?)?,
        })
    })
    .await
    .map_err(|_| "校對工作中斷".to_string())?
}
#[tauri::command]
pub async fn set_review_enabled(
    revision: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<ReviewSettings, String> {
    tauri::async_runtime::spawn_blocking(move || store(&app)?.set_enabled(&revision, enabled))
        .await
        .map_err(|_| "校對工作中斷".to_string())?
}
#[tauri::command]
pub async fn edit_review(
    id: String,
    revision: String,
    change: ReviewChange,
    app: tauri::AppHandle,
) -> Result<Option<ReviewItem>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let store = store(&app)?;
        let now = now()?;
        match change {
            ReviewChange::Save { corrected } => {
                store.review(&id, &revision, corrected, now).map(Some)
            }
            ReviewChange::Promote => store
                .promote(
                    &id,
                    &revision,
                    now,
                    &app.state::<DesktopState>().config_dir.join("vocab.toml"),
                )
                .map(Some),
            ReviewChange::Delete => store.delete(&id, &revision, now).map(|_| None),
        }
    })
    .await
    .map_err(|_| "校對工作中斷".to_string())?
}
#[tauri::command]
pub async fn get_review_audio(
    id: String,
    revision: String,
    app: tauri::AppHandle,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        store(&app)?
            .audio(&id, &revision, now()?)
            .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|_| "校對工作中斷".to_string())?
}

/// Count-only refresh and expiry cleanup while the window is hidden. The tray
/// receives no transcript. Drop signals the thread without waiting on disk IO.
pub struct Monitor(mpsc::Sender<()>);
impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
pub fn monitor(app: tauri::AppHandle, menu: MenuItem<tauri::Wry>) -> std::io::Result<Monitor> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("review-status".into())
        .spawn(move || loop {
            if let Ok(items) = store(&app).and_then(|s| s.list(now()?)) {
                let count = items
                    .iter()
                    .filter(|i| i.status == "pending" || i.promotion_pending)
                    .count();
                let label = format!("抽樣校對 · {count} 筆待處理");
                let _ = menu.set_text(&label);
                if let Some(tray) = app.tray_by_id("main-tray") {
                    let _ = tray.set_tooltip(Some(format!("VoiceType · {count} 筆待校對")));
                }
            }
            match rx.recv_timeout(Duration::from_secs(30)) {
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                _ => break,
            }
        })?;
    Ok(Monitor(tx))
}
