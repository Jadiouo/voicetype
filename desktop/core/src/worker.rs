//! Resident application worker. All process owners are created, polled and
//! destroyed on this thread. Webview commands send work, never own a child.
use crate::{AppError, Application, Provider, Snapshot};
use serde::Serialize;
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRuntimeStatus {
    Inactive,
    WaitingForInput,
    Ready,
    Failed,
}

#[derive(Serialize)]
pub struct DesktopSnapshot {
    pub settings: Snapshot,
    pub local_runtime: LocalRuntimeStatus,
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
                let mut desktop = Desktop {
                    app: Application::open(&config_dir),
                    config_dir,
                    local_status: LocalRuntimeStatus::Inactive,
                    #[cfg(target_os = "linux")]
                    local: None,
                };
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
            if provider != Provider::Local {
                desktop.deactivate()?;
            }
            desktop
                .app_mut()?
                .select_provider(provider)
                .map_err(|e| e.to_string())?;
            desktop.snapshot()
        })
    }

    pub fn reload(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
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
        self.call(move |desktop| {
            if !socket.is_absolute() {
                return Err("本機服務位置必須是完整路徑".into());
            }
            desktop
                .app_mut()?
                .refresh_local_provider(&socket, Duration::from_millis(300));
            desktop.snapshot()
        })
    }

    /// Native setup supplies reviewed installed assets. Not a webview path or
    /// arbitrary executable command; selecting a preference never invokes it.
    #[cfg(target_os = "linux")]
    pub fn activate_local(
        &self,
        paths: crate::runtime::LocalRuntimePaths,
    ) -> Result<DesktopSnapshot, String> {
        self.call(move |desktop| {
            if desktop.app()?.snapshot().dictation.busy {
                return Err(AppError::DictationBusy.to_string());
            }
            if desktop.app()?.snapshot().selected_provider != Provider::Local {
                return Err("請先選擇本機離線辨識".into());
            }
            if desktop.local.is_none() {
                match LocalHost::start(paths) {
                    Ok(local) => {
                        desktop.local = Some(local);
                        desktop.local_status = LocalRuntimeStatus::WaitingForInput;
                    }
                    Err(_) => {
                        desktop.local_status = LocalRuntimeStatus::Failed;
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

    pub fn deactivate_local(&self) -> Result<DesktopSnapshot, String> {
        self.call(|desktop| {
            desktop.deactivate()?;
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
    #[cfg(target_os = "linux")]
    local: Option<LocalHost>,
}

impl Desktop {
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
        })
    }
    fn deactivate(&mut self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        if let Some(local) = self.local.as_mut() {
            let app = self.app.as_mut().map_err(|e| e.to_string())?;
            local
                .shutdown(app)
                .map_err(|_| "本機引擎尚未確認停止，請稍後再試".to_string())?;
            self.local = None;
        }
        self.local_status = LocalRuntimeStatus::Inactive;
        Ok(())
    }
    fn poll(&mut self) {
        #[cfg(target_os = "linux")]
        if let (Some(local), Ok(app)) = (&mut self.local, &mut self.app) {
            match local.poll(app) {
                Ok(status) => self.local_status = status,
                Err(_) => {
                    self.local_status = LocalRuntimeStatus::Failed;
                    if local.shutdown(app).is_ok() {
                        self.local = None;
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
enum EngineOwner {
    Waiting(crate::runtime::OwnedLocal),
    Connected(crate::runtime::OwnedLocalSession),
}

#[cfg(target_os = "linux")]
struct LocalHost {
    owner: Option<EngineOwner>,
    listener: std::os::unix::net::UnixListener,
    endpoint: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl LocalHost {
    fn start(paths: crate::runtime::LocalRuntimePaths) -> io::Result<Self> {
        let endpoint = tempfile::Builder::new()
            .prefix("voicetype-input-")
            .tempdir_in("/tmp")?;
        let listener =
            std::os::unix::net::UnixListener::bind(endpoint.path().join("frontend.sock"))?;
        listener.set_nonblocking(true)?;
        let runtime = crate::runtime::OwnedLocal::start(&paths, Duration::from_secs(30))?;
        Ok(Self {
            owner: Some(EngineOwner::Waiting(runtime)),
            listener,
            endpoint,
        })
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
