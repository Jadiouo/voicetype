//! Resident application worker. All process owners are created, polled and
//! destroyed on this thread. Webview commands send work, never own a child.
use crate::{AppError, Application, Availability, Provider, Snapshot};
use serde::Serialize;
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRuntimeStatus {
    Inactive,
    WaitingForInput,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpellingRuntimeStatus {
    Inactive,
    Disabled,
    Ready,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoogleRuntimeStatus {
    Inactive,
    WaitingForInput,
    TransferPending,
    Ready,
    Prewarming,
    Cleaning,
    Failed,
}

#[derive(Serialize)]
pub struct DesktopSnapshot {
    pub settings: Snapshot,
    pub local_runtime: LocalRuntimeStatus,
    pub spelling_runtime: SpellingRuntimeStatus,
    pub google_runtime: GoogleRuntimeStatus,
    pub google_install: crate::google::release::GoogleInstallInfo,
    pub input_requested: bool,
}

/// Transcripts are fetched separately and never enter diagnostic snapshots or
/// Debug output. The session ID is an opaque string across the JavaScript edge.
#[derive(Serialize)]
pub struct RecoveryText {
    pub provider: Provider,
    pub session: String,
    pub text: String,
    pub reason: crate::DeliveryOutcome,
}

type Job = Box<dyn FnOnce(&mut Desktop) + Send>;
enum Message {
    Call(Job),
    Shutdown(mpsc::SyncSender<Result<(), String>>),
}

pub struct DesktopWorker {
    sender: mpsc::SyncSender<Message>,
    thread: Mutex<Option<JoinHandle<()>>>,
    stopping: AtomicBool,
}

impl DesktopWorker {
    /// Opening settings never launches a runtime or acquires an input endpoint.
    pub fn spawn(config_dir: PathBuf) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(32);
        let thread = thread::Builder::new()
            .name("voicetype-desktop".into())
            .spawn(move || {
                #[cfg(target_os = "linux")]
                let google_install = std::env::var_os("HOME")
                    .map(|home| {
                        crate::google::release::inspect_linux_install(
                            &PathBuf::from(home).join(".local/bin/agy"),
                        )
                    })
                    .unwrap_or(crate::google::release::GoogleInstallInfo {
                        state: crate::google::GoogleInstallState::Missing,
                        version: None,
                    });
                #[cfg(not(target_os = "linux"))]
                let google_install = crate::google::release::GoogleInstallInfo {
                    state: crate::google::GoogleInstallState::Missing,
                    version: None,
                };
                #[cfg(target_os = "linux")]
                let transfer_pending = !matches!(
                    std::fs::symlink_metadata(config_dir.join("google-shortcut-rollback.json")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound
                );
                let mut desktop = Desktop {
                    app: Application::open(&config_dir),
                    config_dir,
                    local_status: LocalRuntimeStatus::Inactive,
                    spelling_status: SpellingRuntimeStatus::Inactive,
                    #[cfg(target_os = "linux")]
                    local: None,
                    #[cfg(target_os = "linux")]
                    google: None,
                    #[cfg(target_os = "linux")]
                    legacy_ownership: None,
                    #[cfg(target_os = "linux")]
                    transfer_pending,
                    google_status: GoogleRuntimeStatus::Inactive,
                    google_install,
                };
                desktop.sync_google_availability();
                loop {
                    match receiver.recv_timeout(Duration::from_millis(10)) {
                        Ok(Message::Call(job)) => job(&mut desktop),
                        Ok(Message::Shutdown(reply)) => {
                            let result = desktop.deactivate();
                            let success = result.is_ok();
                            let _ = reply.send(result);
                            if success {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    desktop.poll();
                }
                let _ = desktop.deactivate();
            })?;
        Ok(Self {
            sender,
            thread: Mutex::new(Some(thread)),
            stopping: AtomicBool::new(false),
        })
    }

    fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Desktop) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        if self.stopping.load(Ordering::Acquire) {
            return Err("App 正在結束，請稍後重新開啟".into());
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Message::Call(Box::new(move |desktop| {
                let _ = sender.send(operation(desktop));
            })))
            .map_err(|_| "App 工作暫時無法接受指令，請稍後再試".to_string())?;
        // Operations have their own bounded IO. Never time out a mutating
        // command here and invite a retry while the original can still execute.
        receiver
            .recv()
            .map_err(|_| "App 工作已中斷，請重新開啟".to_string())?
    }

    pub fn settings(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| desktop.snapshot())
    }

    pub fn recovery(&self) -> Result<Option<RecoveryText>, String> {
        self.call(|desktop| {
            Ok(desktop.app()?.retained_text().map(|text| RecoveryText {
                provider: text.key.provider(),
                session: text.key.id().to_string(),
                text: text.text.clone(),
                reason: text.reason,
            }))
        })
    }

    pub fn dismiss_recovery(&self, provider: Provider, session: String) -> Result<bool, String> {
        self.call(move |desktop| {
            let Ok(session) = session.parse::<u64>() else {
                return Ok(false);
            };
            Ok(desktop.app_mut()?.dismiss_retained_text(provider, session))
        })
    }

    pub fn select_provider(&self, provider: Provider) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            if provider != desktop.app()?.snapshot().selected_provider {
                desktop.deactivate()?;
            }
            desktop
                .app_mut()?
                .select_provider(provider)
                .map_err(|e| e.to_string())?;
            desktop.snapshot()
        })
    }

    pub fn set_spelling_enabled(&self, enabled: bool) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            // The owned worker's configuration is frozen for its lifetime.
            // Keep a loaded/input-routed engine running until explicit unload.
            if desktop.local_status != LocalRuntimeStatus::Inactive
                || desktop.google_status != GoogleRuntimeStatus::Inactive
            {
                return Err("請先停用並卸載引擎，再變更校正設定".into());
            }
            desktop
                .app_mut()?
                .set_spelling_enabled(enabled)
                .map_err(|e| e.to_string())?;
            desktop.snapshot()
        })
    }

    pub fn reload(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            if desktop.local_status != LocalRuntimeStatus::Inactive
                || desktop.google_status != GoogleRuntimeStatus::Inactive
            {
                return Err("請先停用並卸載引擎，再重新載入設定".into());
            }
            match desktop.app.as_mut() {
                Ok(app) => {
                    app.reload().map_err(|e| e.to_string())?;
                }
                Err(_) => {
                    desktop.app = Application::open(&desktop.config_dir);
                }
            }
            if desktop.app()?.snapshot().selected_provider != Provider::Local {
                desktop.deactivate()?;
            }
            desktop.snapshot()
        })
    }

    #[cfg(unix)]
    pub fn refresh_local(&self, socket: PathBuf) -> Result<DesktopSnapshot, String> {
        if !socket.is_absolute() {
            return Err("本機服務位置必須是完整路徑".into());
        }
        let before = self.settings()?;
        if before.settings.dictation.busy || before.google_runtime == GoogleRuntimeStatus::Cleaning
        {
            return Err(AppError::DictationBusy.to_string());
        }
        // The socket may spend its full timeout waiting for pong. Keep that IO
        // outside the resident thread so a hotkey can Start, Stop or Cancel.
        let availability = crate::probe_local_provider(&socket, Duration::from_millis(300));
        self.call(move |desktop| {
            if desktop.app()?.snapshot().dictation.busy
                || desktop.google_status == GoogleRuntimeStatus::Cleaning
            {
                return desktop.snapshot();
            }
            desktop.app_mut()?.set_local_availability(availability);
            desktop.snapshot()
        })
    }

    /// Read-only exact-image inspection. Login and audio remain unchecked.
    #[cfg(target_os = "linux")]
    pub fn refresh_google_install(&self, executable: PathBuf) -> Result<DesktopSnapshot, String> {
        self.refresh_google_install_with(executable, crate::google::release::inspect_linux_install)
    }

    #[cfg(target_os = "linux")]
    fn refresh_google_install_with(
        &self,
        executable: PathBuf,
        inspect: impl FnOnce(&std::path::Path) -> crate::google::release::GoogleInstallInfo,
    ) -> Result<DesktopSnapshot, String> {
        // The image is hundreds of MB. Never read or hash it on the resident
        // thread, where Fcitx Stop/Cancel and recorder polling must progress.
        let before = self.settings()?;
        if before.settings.dictation.busy || before.google_runtime == GoogleRuntimeStatus::Cleaning
        {
            return Err(AppError::DictationBusy.to_string());
        }
        let inspected = inspect(&executable);
        self.call(move |desktop| {
            // A hotkey may have started a session while inspection ran. Keep
            // its current status and let the user refresh again when idle.
            if desktop.app()?.snapshot().dictation.busy
                || desktop.google_status == GoogleRuntimeStatus::Cleaning
            {
                return desktop.snapshot();
            }
            desktop.google_install = inspected;
            desktop.sync_google_availability();
            desktop.snapshot()
        })
    }

    /// Native setup supplies reviewed installed assets. Not a webview path or
    /// arbitrary executable command; selecting a preference never invokes it.
    #[cfg(target_os = "linux")]
    pub fn activate_local(
        &self,
        mut paths: crate::runtime::LocalRuntimePaths,
    ) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.transfer_pending {
                return Err("Google 快捷鍵交接尚未完成；請先切換或還原".into());
            }
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            if desktop.app()?.snapshot().selected_provider != Provider::Local {
                return Err("請先選擇本機離線辨識".into());
            }
            if desktop.google.is_some() {
                return Err("請先確認 Google 錄音完全停止，再載入本機引擎".into());
            }
            if desktop.local.is_none() {
                let spelling_enabled = desktop.app()?.snapshot().spelling_enabled;
                if !spelling_enabled {
                    paths.spelling = None;
                }
                let spelling_requested = spelling_enabled && paths.spelling.is_some();
                match LocalHost::start(paths) {
                    Ok(local) => {
                        desktop.spelling_status = if !spelling_enabled {
                            SpellingRuntimeStatus::Disabled
                        } else if spelling_requested && local.spelling_ready {
                            SpellingRuntimeStatus::Ready
                        } else {
                            SpellingRuntimeStatus::Unavailable
                        };
                        desktop.local = Some(local);
                        desktop.local_status = LocalRuntimeStatus::WaitingForInput;
                    }
                    Err(_) => {
                        desktop.local_status = LocalRuntimeStatus::Failed;
                        desktop.spelling_status = if spelling_enabled {
                            SpellingRuntimeStatus::Unavailable
                        } else {
                            SpellingRuntimeStatus::Disabled
                        };
                        return Err("無法準備本機引擎，請檢查執行環境與模型安裝".into());
                    }
                }
            }
            desktop.snapshot()
        })
    }

    /// Endpoint is for the native input integration/migration controller, not
    /// for web content. Its private directory has mode 0700 and a single owner.
    #[cfg(target_os = "linux")]
    pub fn local_endpoint(&self) -> Result<Option<PathBuf>, String> {
        self.call(|desktop| {
            Ok(desktop
                .local
                .as_ref()
                .map(|local| local.endpoint.path().join("frontend.sock")))
        })
    }

    /// The factory runs on the resident thread. It must authenticate the
    /// reviewed official CLI and empty editor before publishing an input route.
    #[cfg(target_os = "linux")]
    pub fn activate_google<B, F, M>(
        &self,
        runtime_dir: PathBuf,
        create: F,
        format: M,
        spelling_ready: bool,
        review: Option<std::sync::Arc<crate::review_collector::Collector>>,
    ) -> Result<DesktopSnapshot, String>
    where
        B: crate::google::GoogleBoundary + Send + 'static,
        F: FnMut() -> io::Result<B> + Send + 'static,
        M: Fn(String) -> io::Result<String> + Send + 'static,
    {
        self.activate_google_mode(runtime_dir, create, format, spelling_ready, review, true)
    }

    /// Preclaim route accepts the v2 identity handshake, but its resident
    /// dispatcher rejects Start until the legacy bridge ownership is installed.
    #[cfg(target_os = "linux")]
    pub fn prepare_google_transfer<B, F, M>(
        &self,
        runtime_dir: PathBuf,
        create: F,
        format: M,
        spelling_ready: bool,
        review: Option<std::sync::Arc<crate::review_collector::Collector>>,
    ) -> Result<DesktopSnapshot, String>
    where
        B: crate::google::GoogleBoundary + Send + 'static,
        F: FnMut() -> io::Result<B> + Send + 'static,
        M: Fn(String) -> io::Result<String> + Send + 'static,
    {
        self.activate_google_mode(runtime_dir, create, format, spelling_ready, review, false)
    }

    #[cfg(target_os = "linux")]
    pub fn begin_google_transfer_setup(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            if desktop.app()?.snapshot().selected_provider != Provider::Google {
                return Err("請先選擇 Google 官方辨識".into());
            }
            if desktop.app()?.snapshot().dictation.busy
                || desktop.local.is_some()
                || desktop.google.is_some()
                || desktop.legacy_ownership.is_some()
                || desktop.local_status != LocalRuntimeStatus::Inactive
                || desktop.google_status != GoogleRuntimeStatus::Inactive
            {
                return Err("請先結束聽寫並卸載 App 引擎，再準備切換".into());
            }
            desktop.transfer_pending = true;
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    pub fn finish_google_transfer_setup(
        &self,
        transfer: crate::google::legacy_transfer::GoogleShortcutTransfer,
    ) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.local.is_some()
                || desktop.google.is_some()
                || desktop.app()?.snapshot().dictation.busy
            {
                return Err("Google 引擎尚未完全清理".into());
            }
            if transfer.phase().map_err(|e| e.to_string())?.is_some() {
                return Err("Google 快捷鍵還原仍待 Fcitx 重新載入".into());
            }
            desktop.legacy_ownership = None;
            desktop.transfer_pending = false;
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    fn activate_google_mode<B, F, M>(
        &self,
        runtime_dir: PathBuf,
        create: F,
        format: M,
        spelling_ready: bool,
        review: Option<std::sync::Arc<crate::review_collector::Collector>>,
        allow_start: bool,
    ) -> Result<DesktopSnapshot, String>
    where
        B: crate::google::GoogleBoundary + Send + 'static,
        F: FnMut() -> io::Result<B> + Send + 'static,
        M: Fn(String) -> io::Result<String> + Send + 'static,
    {
        self.call(move |desktop| {
            if desktop.transfer_pending != !allow_start {
                return Err("Google 交接狀態不符；錄音未開始".into());
            }
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            if desktop.app()?.snapshot().selected_provider != Provider::Google {
                return Err("請先選擇 Google 官方辨識".into());
            }
            if desktop.local.is_some()
                || desktop.google.is_some()
                || desktop.legacy_ownership.is_some()
            {
                return Err("請先卸載目前的聽寫引擎".into());
            }
            match GoogleHost::start(&runtime_dir, create, format, review, allow_start) {
                Ok(host) => {
                    desktop.google = Some(host);
                    desktop.google_status = GoogleRuntimeStatus::WaitingForInput;
                    desktop.spelling_status = if !desktop.app()?.snapshot().spelling_enabled {
                        SpellingRuntimeStatus::Disabled
                    } else if spelling_ready {
                        SpellingRuntimeStatus::Ready
                    } else {
                        SpellingRuntimeStatus::Unavailable
                    };
                    desktop.sync_google_availability();
                }
                Err(_) => {
                    desktop.google_status = GoogleRuntimeStatus::Failed;
                    desktop.sync_google_availability();
                    return Err("Google 官方 CLI、登入或輸入環境尚未通過檢查；未啟動錄音".into());
                }
            }
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    pub fn complete_google_transfer(
        &self,
        ownership: crate::google::legacy_transfer::LegacyOwnership,
        expected_fcitx: crate::google::legacy_transfer::FcitxIdentity,
    ) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                desktop.legacy_ownership = Some(ownership);
                return Err("Google 交接路由狀態不符；錄音仍鎖定".into());
            }
            let Some(host) = desktop.google.as_mut() else {
                desktop.legacy_ownership = Some(ownership);
                return Err("Google 交接路由已中斷；服務鎖仍由 App 持有".into());
            };
            if host.start_permit.load(Ordering::Acquire)
                || host.legacy_ownership.is_some()
                || !matches!(host.owner.as_ref(), Some(GoogleOwner::Connected(dispatcher))
                    if dispatcher.peer_pid() == expected_fcitx.pid)
            {
                desktop.legacy_ownership = Some(ownership);
                return Err("Google 交接路由狀態不符；錄音仍鎖定".into());
            }
            host.legacy_ownership = Some(ownership);
            host.start_permit.store(true, Ordering::Release);
            desktop.google_status = GoogleRuntimeStatus::Ready;
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    pub fn park_google_transfer_ownership(
        &self,
        ownership: crate::google::legacy_transfer::LegacyOwnership,
    ) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if let Some(host) = desktop.google.as_mut() {
                if host.legacy_ownership.is_some() {
                    return Err("服務鎖已由 App 持有".into());
                }
                host.start_permit.store(false, Ordering::Release);
                host.legacy_ownership = Some(ownership);
            } else {
                if desktop.legacy_ownership.is_some() {
                    return Err("服務鎖已由 App 持有".into());
                }
                desktop.legacy_ownership = Some(ownership);
            }
            desktop.google_status = GoogleRuntimeStatus::TransferPending;
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    pub fn google_transfer_owned(&self) -> Result<bool, String> {
        self.call(|desktop| {
            Ok(desktop.legacy_ownership.is_some()
                || desktop
                    .google
                    .as_ref()
                    .is_some_and(|host| host.legacy_ownership.is_some()))
        })
    }

    #[cfg(target_os = "linux")]
    pub fn google_transfer_route_ready(&self) -> Result<bool, String> {
        self.call(|desktop| {
            Ok(desktop.google.as_ref().is_some_and(|host| {
                !host.start_permit.load(Ordering::Acquire)
                    && matches!(host.owner, Some(GoogleOwner::Connected(_)))
            }))
        })
    }

    #[cfg(target_os = "linux")]
    pub fn google_transfer_route_pid(&self) -> Result<Option<u32>, String> {
        self.call(|desktop| {
            Ok(desktop.google.as_ref().and_then(|host| {
                if host.start_permit.load(Ordering::Acquire) {
                    return None;
                }
                match host.owner.as_ref() {
                    Some(GoogleOwner::Connected(dispatcher)) => Some(dispatcher.peer_pid()),
                    _ => None,
                }
            }))
        })
    }

    /// Close the resident Start gate before draining the route for restore.
    #[cfg(target_os = "linux")]
    pub fn begin_google_transfer_restore(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            let host = desktop.google.as_mut().ok_or("Google 引擎未載入")?;
            if host.legacy_ownership.is_none() && desktop.legacy_ownership.is_none() {
                return Err("目前沒有舊 Google 服務的交接所有權".into());
            }
            host.start_permit.store(false, Ordering::Release);
            desktop.google_status = GoogleRuntimeStatus::TransferPending;
            desktop.snapshot()
        })
    }

    /// Called only after deactivate_google has reaped the route and child.
    /// The lock remains in resident state if restore refuses external drift.
    #[cfg(target_os = "linux")]
    pub fn restore_google_transfer<S>(
        &self,
        transfer: crate::google::legacy_transfer::GoogleShortcutTransfer,
        service: S,
    ) -> Result<DesktopSnapshot, String>
    where
        S: crate::google::legacy_transfer::LegacyService + Send + 'static,
    {
        self.call(move |desktop| {
            if desktop.google.is_some()
                || desktop.google_status != GoogleRuntimeStatus::Inactive
                || desktop.app()?.snapshot().dictation.busy
            {
                return Err("Google 錄音或清理尚未完成".into());
            }
            let ownership = desktop
                .legacy_ownership
                .as_mut()
                .ok_or("沒有可還原的服務鎖")?;
            transfer
                .stage_restore(ownership, true, &service)
                .map_err(|e| e.to_string())?;
            desktop.legacy_ownership = None;
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    pub fn google_endpoint(&self) -> Result<Option<PathBuf>, String> {
        self.call(|desktop| {
            Ok(desktop
                .google
                .as_ref()
                .map(|host| host.endpoint.path().join("frontend.sock")))
        })
    }

    #[cfg(target_os = "linux")]
    pub fn deactivate_google(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            desktop.deactivate()?;
            desktop.snapshot()
        })
    }

    /// Explicit native input opt-in. The shell supplies its login runtime
    /// directory; the webview cannot choose a socket, executable or file path.
    #[cfg(target_os = "linux")]
    pub fn enable_local_input(&self, runtime_dir: PathBuf) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.transfer_pending {
                return Err("Google 快捷鍵交接尚未完成；請先切換或還原".into());
            }
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            let local = desktop.local.as_mut().ok_or("請先載入本機引擎")?;
            if local.input.is_none() {
                local.input = Some(
                    crate::input_lease::InputLease::acquire(
                        &runtime_dir,
                        &local.endpoint.path().join("frontend.sock"),
                    )
                    .map_err(|_| {
                        "無法接管輸入法：請確認登入環境，並關閉其他 VoiceType App".to_string()
                    })?,
                );
            }
            desktop.snapshot()
        })
    }

    #[cfg(target_os = "linux")]
    pub fn configure_input_module(
        &self,
        installer: crate::input_install::FcitxInstaller,
        source: Option<PathBuf>,
    ) -> Result<bool, String> {
        self.call(move |desktop| {
            if desktop.app()?.snapshot().dictation.busy
                || desktop
                    .local
                    .as_ref()
                    .is_some_and(|local| local.input.is_some())
                || desktop.google.is_some()
            {
                return Err("請先停用 App 聽寫，再變更輸入法模組".into());
            }
            match source {
                Some(source) => installer.install(&source).map(|_| true),
                None => installer.restore(),
            }
            .map_err(|_| {
                "無法變更輸入法模組：原設定已保留；請檢查是否有外部修改或安裝檔損壞".into()
            })
        })
    }

    pub fn deactivate_local(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            desktop.deactivate()?;
            desktop.snapshot()
        })
    }

    pub fn cancel_dictation(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            #[cfg(target_os = "linux")]
            if let (Some(local), Ok(app)) = (&mut desktop.local, &mut desktop.app) {
                if let Some(EngineOwner::Connected(session)) = local.owner.as_mut() {
                    session.cancel(app);
                }
            }
            #[cfg(target_os = "linux")]
            if let (Some(google), Ok(app)) = (&mut desktop.google, &mut desktop.app) {
                google.cancel(app);
            }
            desktop.snapshot()
        })
    }

    /// Success means the worker has reaped its child and exited. Run off the UI
    /// event thread. A failed cleanup leaves the worker available for recovery.
    pub fn shutdown(&self) -> Result<(), String> {
        let mut owner = self.thread.lock().map_err(|_| "App 工作執行緒已中斷")?;
        let Some(thread) = owner.as_ref() else {
            return Ok(());
        };
        self.stopping.store(true, Ordering::Release);
        if !thread.is_finished() {
            let (sender, receiver) = mpsc::sync_channel(1);
            if self.sender.send(Message::Shutdown(sender)).is_ok() {
                if let Ok(Err(error)) = receiver.recv() {
                    self.stopping.store(false, Ordering::Release);
                    return Err(error);
                }
            }
        }
        owner
            .take()
            .unwrap()
            .join()
            .map_err(|_| "App 工作執行緒已中斷".into())
    }
}

impl Drop for DesktopWorker {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Desktop {
    config_dir: PathBuf,
    app: Result<Application, AppError>,
    local_status: LocalRuntimeStatus,
    spelling_status: SpellingRuntimeStatus,
    google_status: GoogleRuntimeStatus,
    google_install: crate::google::release::GoogleInstallInfo,
    #[cfg(target_os = "linux")]
    local: Option<LocalHost>,
    #[cfg(target_os = "linux")]
    google: Option<GoogleHost>,
    #[cfg(target_os = "linux")]
    legacy_ownership: Option<crate::google::legacy_transfer::LegacyOwnership>,
    #[cfg(target_os = "linux")]
    transfer_pending: bool,
}

impl Desktop {
    fn sync_google_availability(&mut self) {
        use crate::google::GoogleInstallState;
        let availability = match self.google_status {
            GoogleRuntimeStatus::Ready => Availability::ServiceAvailable,
            GoogleRuntimeStatus::Failed => Availability::Offline,
            GoogleRuntimeStatus::WaitingForInput
            | GoogleRuntimeStatus::TransferPending
            | GoogleRuntimeStatus::Prewarming
            | GoogleRuntimeStatus::Cleaning => Availability::NotConnected,
            GoogleRuntimeStatus::Inactive => match self.google_install.state {
                GoogleInstallState::Missing => Availability::Offline,
                GoogleInstallState::Incompatible => Availability::Incompatible,
                GoogleInstallState::OfficialCheckRequired => Availability::NotConnected,
            },
        };
        if let Ok(app) = self.app.as_mut() {
            app.set_google_availability(availability);
        }
    }
    fn app(&self) -> Result<&Application, String> {
        self.app.as_ref().map_err(ToString::to_string)
    }
    fn app_mut(&mut self) -> Result<&mut Application, String> {
        self.app.as_mut().map_err(|e| e.to_string())
    }
    fn snapshot(&self) -> Result<DesktopSnapshot, String> {
        Ok(DesktopSnapshot {
            settings: self.app()?.snapshot(),
            local_runtime: self.local_status,
            spelling_runtime: self.spelling_status,
            google_runtime: self.google_status,
            google_install: self.google_install,
            input_requested: {
                #[cfg(target_os = "linux")]
                {
                    self.local
                        .as_ref()
                        .is_some_and(|local| local.input.is_some())
                        || self.google.is_some()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    false
                }
            },
        })
    }
    fn deactivate(&mut self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        if let Some(google) = self.google.as_mut() {
            let app = self.app.as_mut().map_err(|e| e.to_string())?;
            google.shutdown(app).map_err(|_| {
                self.google_status = GoogleRuntimeStatus::Cleaning;
                "Google 錄音尚未確認停止，麥克風仍保留；請稍後再試".to_string()
            })?;
            if let Some(ownership) = google.legacy_ownership.take() {
                self.legacy_ownership = Some(ownership);
            }
            self.google = None;
        }
        #[cfg(target_os = "linux")]
        if let Some(local) = self.local.as_mut() {
            let app = self.app.as_mut().map_err(|e| e.to_string())?;
            local
                .shutdown(app)
                .map_err(|_| "本機引擎尚未確認停止，請稍後再試".to_string())?;
            self.local = None;
        }
        self.local_status = LocalRuntimeStatus::Inactive;
        self.spelling_status = SpellingRuntimeStatus::Inactive;
        self.google_status = GoogleRuntimeStatus::Inactive;
        self.sync_google_availability();
        Ok(())
    }
    fn poll(&mut self) {
        #[cfg(target_os = "linux")]
        if let (Some(local), Ok(app)) = (&mut self.local, &mut self.app) {
            match local.poll(app) {
                Ok(status) => {
                    self.local_status = status;
                    if self.spelling_status == SpellingRuntimeStatus::Ready {
                        local.refresh_spelling(app);
                        if !local.spelling_ready {
                            self.spelling_status = SpellingRuntimeStatus::Unavailable;
                        }
                    }
                }
                Err(_) => {
                    self.local_status = LocalRuntimeStatus::Failed;
                    if self.spelling_status == SpellingRuntimeStatus::Ready {
                        self.spelling_status = SpellingRuntimeStatus::Unavailable;
                    }
                    if local.shutdown(app).is_ok() {
                        self.local = None;
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        if let (Some(google), Ok(app)) = (&mut self.google, &mut self.app) {
            self.google_status = match google.poll(app) {
                Ok(status) => status,
                Err(_) => {
                    let _ = google.shutdown(app);
                    if google.released.load(Ordering::Acquire) {
                        GoogleRuntimeStatus::Failed
                    } else {
                        GoogleRuntimeStatus::Cleaning
                    }
                }
            };
            if google.released.load(Ordering::Acquire) && google.owner.is_none() {
                if let Some(ownership) = google.legacy_ownership.take() {
                    self.legacy_ownership = Some(ownership);
                }
                self.google = None;
            }
        }
        self.sync_google_availability();
    }
}

#[cfg(target_os = "linux")]
enum EngineOwner {
    Waiting(crate::runtime::OwnedLocal),
    Connected(crate::runtime::OwnedLocalSession),
}

#[cfg(target_os = "linux")]
struct LocalHost {
    input: Option<crate::input_lease::InputLease>,
    owner: Option<EngineOwner>,
    listener: std::os::unix::net::UnixListener,
    endpoint: tempfile::TempDir,
    spelling_ready: bool,
    next_spelling_probe: Instant,
    spelling_probe: Option<JoinHandle<io::Result<bool>>>,
}

#[cfg(target_os = "linux")]
impl LocalHost {
    fn start(paths: crate::runtime::LocalRuntimePaths) -> io::Result<Self> {
        let endpoint = tempfile::Builder::new()
            .prefix("voicetype-input-")
            .tempdir_in(std::env::temp_dir())?;
        let listener =
            std::os::unix::net::UnixListener::bind(endpoint.path().join("frontend.sock"))?;
        listener.set_nonblocking(true)?;
        let startup_budget = if paths.spelling.is_some() { 90 } else { 30 };
        let runtime =
            crate::runtime::OwnedLocal::start(&paths, Duration::from_secs(startup_budget))?;
        let spelling_ready = runtime.spelling_ready();
        Ok(Self {
            input: None,
            owner: Some(EngineOwner::Waiting(runtime)),
            listener,
            endpoint,
            spelling_ready,
            next_spelling_probe: Instant::now() + Duration::from_millis(250),
            spelling_probe: None,
        })
    }

    fn refresh_spelling(&mut self, app: &Application) {
        if self
            .spelling_probe
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let result = self.spelling_probe.take().unwrap().join();
            // A transient timeout or background panic cannot prove CSC died.
            if let Ok(Ok(ready)) = result {
                self.spelling_ready = ready;
            }
        }
        if !self.spelling_ready
            || app.snapshot().dictation.busy
            || Instant::now() < self.next_spelling_probe
            || self.spelling_probe.is_some()
        {
            return;
        }
        self.next_spelling_probe = Instant::now() + Duration::from_millis(500);
        let result = match self.owner.as_ref() {
            Some(EngineOwner::Waiting(runtime)) => {
                runtime.spawn_spelling_probe(Duration::from_millis(50))
            }
            Some(EngineOwner::Connected(session)) => {
                session.spawn_spelling_probe(Duration::from_millis(50))
            }
            None => return,
        };
        self.spelling_probe = result.ok();
    }

    fn poll(&mut self, app: &mut Application) -> io::Result<LocalRuntimeStatus> {
        match self.owner.as_mut().ok_or(io::ErrorKind::NotConnected)? {
            EngineOwner::Waiting(runtime) => {
                if !runtime.is_running()? {
                    return Err(io::Error::other("local engine exited"));
                }
                match self.listener.accept() {
                    Ok((peer, _)) => {
                        let Some(EngineOwner::Waiting(runtime)) = self.owner.take() else {
                            unreachable!();
                        };
                        let session = runtime.attach_frontend(peer, Duration::from_millis(300))?;
                        self.owner = Some(EngineOwner::Connected(session));
                        Ok(LocalRuntimeStatus::Ready)
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        Ok(LocalRuntimeStatus::WaitingForInput)
                    }
                    Err(error) => Err(error),
                }
            }
            EngineOwner::Connected(session) => {
                session.step(app, Duration::from_millis(10))?;
                Ok(LocalRuntimeStatus::Ready)
            }
        }
    }

    fn shutdown(&mut self, app: &mut Application) -> io::Result<()> {
        self.input = None;
        if let Some(probe) = self.spelling_probe.take() {
            let _ = probe.join();
        }
        if let Some(owner) = self.owner.as_mut() {
            match owner {
                EngineOwner::Waiting(runtime) => runtime.shutdown()?,
                EngineOwner::Connected(session) => session.shutdown(app)?,
            }
        }
        self.owner = None;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for LocalHost {
    fn drop(&mut self) {
        if let Some(probe) = self.spelling_probe.take() {
            let _ = probe.join();
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod refresh_tests {
    use super::*;

    #[test]
    fn blocked_image_inspection_does_not_block_resident_worker() {
        use crate::google::{release::GoogleInstallInfo, GoogleInstallState};

        let root = tempfile::tempdir().unwrap();
        let worker = DesktopWorker::spawn(root.path().join("settings")).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        thread::scope(|scope| {
            let executable = root.path().join("cli");
            let worker_ref = &worker;
            let inspection = scope.spawn(move || {
                worker_ref.refresh_google_install_with(executable, move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    GoogleInstallInfo {
                        state: GoogleInstallState::Incompatible,
                        version: None,
                    }
                })
            });
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let (reply_tx, reply_rx) = mpsc::channel();
            scope.spawn(move || {
                reply_tx
                    .send(worker_ref.settings().map(|s| s.google_runtime))
                    .unwrap();
            });
            let observed = reply_rx.recv_timeout(Duration::from_secs(2));
            release_tx.send(()).unwrap();
            assert_eq!(observed.unwrap().unwrap(), GoogleRuntimeStatus::Inactive);
            inspection.join().unwrap().unwrap();
        });
        worker.shutdown().unwrap();
    }
}

#[cfg(target_os = "linux")]
struct LeasedGoogle<B: crate::google::GoogleBoundary, F: FnMut() -> io::Result<B>> {
    boundary: B,
    reopen: F,
    closed: bool,
    cancellation: Option<std::sync::Arc<AtomicBool>>,
    lease: Option<crate::input_lease::InputLease>,
    released: std::sync::Arc<AtomicBool>,
}

#[cfg(target_os = "linux")]
impl<B: crate::google::GoogleBoundary, F: FnMut() -> io::Result<B>> Drop for LeasedGoogle<B, F> {
    fn drop(&mut self) {
        // The worker drops this only after verified cleanup. Keep the route
        // lock through every failed retry, including a detached worker.
        drop(self.lease.take());
        self.released.store(true, Ordering::Release);
    }
}

#[cfg(target_os = "linux")]
impl<B: crate::google::GoogleBoundary, F: FnMut() -> io::Result<B>> crate::google::GoogleBoundary
    for LeasedGoogle<B, F>
{
    fn set_cancellation(&mut self, flag: std::sync::Arc<AtomicBool>) {
        self.boundary.set_cancellation(flag.clone());
        self.cancellation = Some(flag);
    }
    fn start_capture(&mut self) -> io::Result<()> {
        self.prewarm_next()?;
        self.boundary.start_capture()
    }
    fn stop_capture_and_drain(&mut self) -> io::Result<()> {
        self.boundary.stop_capture_and_drain()
    }
    fn stop_official_voice(&mut self) -> io::Result<()> {
        self.boundary.stop_official_voice()
    }
    fn wait_official_recorder(&mut self) -> io::Result<()> {
        self.boundary.wait_official_recorder()
    }
    fn capture_editor(&mut self) -> io::Result<String> {
        self.boundary.capture_editor()
    }
    fn settle(&mut self, duration: Duration) -> io::Result<()> {
        self.boundary.settle(duration)
    }
    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        self.boundary.reject_known_voice_errors()
    }
    fn cleanup(&mut self) -> io::Result<()> {
        self.boundary.cleanup()?;
        self.closed = true;
        Ok(())
    }
    fn take_review_pcm(&mut self) -> Option<Vec<u8>> {
        self.boundary.take_review_pcm()
    }
    fn prewarm_next(&mut self) -> io::Result<()> {
        if self.closed {
            let mut replacement = (self.reopen)()?;
            if let Some(flag) = &self.cancellation {
                replacement.set_cancellation(flag.clone());
            }
            self.boundary = replacement;
            self.closed = false;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
struct GoogleHost {
    owner: Option<GoogleOwner>,
    listener: std::os::unix::net::UnixListener,
    endpoint: tempfile::TempDir,
    released: std::sync::Arc<AtomicBool>,
    pending_release: Option<crate::SessionKey>,
    review: Option<std::sync::Arc<crate::review_collector::Collector>>,
    start_permit: std::sync::Arc<AtomicBool>,
    legacy_ownership: Option<crate::google::legacy_transfer::LegacyOwnership>,
}

#[cfg(target_os = "linux")]
enum GoogleOwner {
    Waiting(crate::google::GoogleProvider),
    Connected(crate::dispatch::GoogleDispatcher),
}

#[cfg(target_os = "linux")]
struct NoDelivery;
#[cfg(target_os = "linux")]
impl crate::DeliveryPort for NoDelivery {
    fn commit_if_focused(&mut self, _: &crate::TargetLease, _: &str) -> crate::DeliveryOutcome {
        crate::DeliveryOutcome::FocusChanged
    }
}

#[cfg(target_os = "linux")]
impl GoogleHost {
    fn start<B, F, M>(
        runtime_dir: &std::path::Path,
        mut create: F,
        format: M,
        review: Option<std::sync::Arc<crate::review_collector::Collector>>,
        allow_start: bool,
    ) -> io::Result<Self>
    where
        B: crate::google::GoogleBoundary + Send + 'static,
        F: FnMut() -> io::Result<B> + Send + 'static,
        M: Fn(String) -> io::Result<String> + Send + 'static,
    {
        let endpoint = tempfile::Builder::new()
            .prefix("voicetype-google-input-")
            .tempdir_in(std::env::temp_dir())?;
        let listener =
            std::os::unix::net::UnixListener::bind(endpoint.path().join("frontend.sock"))?;
        listener.set_nonblocking(true)?;
        let boundary = create()?;
        let lease = crate::input_lease::InputLease::acquire_google(
            runtime_dir,
            &endpoint.path().join("frontend.sock"),
        )?;
        let released = std::sync::Arc::new(AtomicBool::new(false));
        let provider = crate::google::GoogleProvider::spawn_with_text(
            LeasedGoogle {
                boundary,
                reopen: create,
                closed: false,
                cancellation: None,
                lease: Some(lease),
                released: released.clone(),
            },
            format,
        )?;
        Ok(Self {
            owner: Some(GoogleOwner::Waiting(provider)),
            listener,
            endpoint,
            released,
            pending_release: None,
            review,
            start_permit: std::sync::Arc::new(AtomicBool::new(allow_start)),
            legacy_ownership: None,
        })
    }

    fn poll(&mut self, app: &mut Application) -> io::Result<GoogleRuntimeStatus> {
        match self.owner.as_mut() {
            Some(GoogleOwner::Waiting(_)) => match self.listener.accept() {
                Ok((peer, _)) => {
                    let Some(GoogleOwner::Waiting(provider)) = self.owner.take() else {
                        unreachable!()
                    };
                    let dispatcher = crate::dispatch::GoogleDispatcher::accept(
                        peer,
                        provider,
                        Duration::from_millis(300),
                        self.review.clone(),
                        self.start_permit.clone(),
                    )?;
                    self.owner = Some(GoogleOwner::Connected(dispatcher));
                    Ok(if self.start_permit.load(Ordering::Acquire) {
                        GoogleRuntimeStatus::Ready
                    } else {
                        GoogleRuntimeStatus::TransferPending
                    })
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    Ok(GoogleRuntimeStatus::WaitingForInput)
                }
                Err(error) => Err(error),
            },
            Some(GoogleOwner::Connected(dispatcher)) => {
                dispatcher.step(app, Duration::from_millis(10))?;
                if !self.start_permit.load(Ordering::Acquire) {
                    return Ok(GoogleRuntimeStatus::TransferPending);
                }
                Ok(match dispatcher.warm_state() {
                    1 => GoogleRuntimeStatus::Prewarming,
                    2 => GoogleRuntimeStatus::Failed,
                    _ => GoogleRuntimeStatus::Ready,
                })
            }
            None if self.released.load(Ordering::Acquire) => {
                self.release_pending(app);
                Ok(GoogleRuntimeStatus::Inactive)
            }
            None => Ok(GoogleRuntimeStatus::Cleaning),
        }
    }

    fn cancel(&mut self, app: &mut Application) {
        if let Some(GoogleOwner::Connected(dispatcher)) = self.owner.as_mut() {
            dispatcher.cancel(app);
        }
    }

    fn release_pending(&mut self, app: &mut Application) {
        if let Some(key) = self.pending_release.take() {
            app.provider_event(key, crate::ProviderEvent::Released, &mut NoDelivery);
        }
    }

    fn shutdown(&mut self, app: &mut Application) -> io::Result<()> {
        if let Some(owner) = self.owner.take() {
            match owner {
                GoogleOwner::Waiting(provider) => drop(provider),
                GoogleOwner::Connected(mut dispatcher) => {
                    dispatcher.cancel(app);
                    self.pending_release = dispatcher.active_key();
                    drop(dispatcher);
                }
            }
        }
        if self.released.load(Ordering::Acquire) {
            self.release_pending(app);
            Ok(())
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}
