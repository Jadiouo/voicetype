//! UI-facing commands, independent of the webview and platform audio libraries.

mod dictation;
#[cfg(target_os = "linux")]
pub mod dispatch;
#[cfg(target_os = "linux")]
pub mod local;
#[cfg(unix)]
mod providers;
#[cfg(target_os = "linux")]
pub mod runtime;
pub mod worker;
pub use dictation::{
    CommandRejected, DeliveryOutcome, DeliveryPort, DictationContext, DictationPhase,
    DictationStatus, ProviderCommand, ProviderEvent, ProviderPort, RetainedText, SessionClock,
    SessionFailure, SessionKey, TargetLease,
};

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Local,
    Google,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    NotConnected,
    ServiceAvailable,
    Offline,
    TimedOut,
    Incompatible,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub availability: Availability,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub dictation: DictationStatus,
    pub selected_provider: Provider,
    pub providers: Vec<ProviderStatus>,
    pub compute_device: &'static str,
    pub preview: bool,
    pub version: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("無法存取設定：{0}")]
    Io(#[from] std::io::Error),
    #[error("設定格式無效，原檔已保留：{0}")]
    InvalidConfig(#[from] serde_json::Error),
    #[error("設定版本 {0} 不受支援；請使用相容版本，原檔已保留")]
    UnsupportedSchema(u32),
    #[error("設定已被另一個視窗或程式修改，請重新載入後再儲存")]
    ConfigConflict,
    #[error("另一個程式正在儲存設定，請稍後再試")]
    SettingsBusy,
    #[error("錄音或辨識尚未結束，請先完成或取消目前工作")]
    DictationBusy,
    #[error("辨識引擎尚未就緒或未接受指令；未切換到其他引擎")]
    ProviderUnavailable,
    #[error("工作階段編號已用盡，請重新開啟 App")]
    SessionIdsExhausted,
}

#[derive(Clone, Deserialize, Serialize)]
struct Preferences {
    schema_version: u32,
    selected_provider: Provider,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            selected_provider: Provider::Local,
            extra: Default::default(),
        }
    }
}

pub struct Application {
    path: PathBuf,
    preferences: Preferences,
    original: Option<Vec<u8>>,
    local_status: Availability,
    dictation: dictation::Coordinator,
}

impl Application {
    /// Loading settings never launches a provider, opens a microphone or logs in.
    pub fn open(config_dir: &Path) -> Result<Self, AppError> {
        Self::open_with_coordinator(config_dir, dictation::Coordinator::default())
    }

    pub fn open_with_clock(
        config_dir: &Path,
        clock: std::sync::Arc<dyn SessionClock>,
    ) -> Result<Self, AppError> {
        Self::open_with_coordinator(config_dir, dictation::Coordinator::with_clock(clock))
    }

    fn open_with_coordinator(
        config_dir: &Path,
        coordinator: dictation::Coordinator,
    ) -> Result<Self, AppError> {
        let path = config_dir.join("desktop.json");
        let original = read_optional(&path)?;
        let preferences: Preferences = match &original {
            Some(data) => serde_json::from_slice(data)?,
            None => Preferences::default(),
        };
        if preferences.schema_version != 1 {
            return Err(AppError::UnsupportedSchema(preferences.schema_version));
        }
        Ok(Self {
            path,
            preferences,
            original,
            local_status: Availability::NotConnected,
            dictation: coordinator,
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            dictation: self.dictation.status(),
            selected_provider: self.preferences.selected_provider,
            providers: [Provider::Local, Provider::Google]
                .into_iter()
                .map(|provider| ProviderStatus {
                    provider,
                    availability: if provider == Provider::Local {
                        self.local_status
                    } else {
                        Availability::NotConnected
                    },
                })
                .collect(),
            compute_device: "cpu",
            preview: true,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    pub fn reload(&mut self) -> Result<Snapshot, AppError> {
        if self.dictation.busy() {
            return Err(AppError::DictationBusy);
        }
        let loaded = Self::open(self.path.parent().expect("config file has a parent"))?;
        self.preferences = loaded.preferences;
        self.original = loaded.original;
        Ok(self.snapshot())
    }

    /// Read-only probe of a loaded local engine. Call off the UI/event thread.
    #[cfg(unix)]
    pub fn refresh_local_provider(
        &mut self,
        socket: &Path,
        budget: std::time::Duration,
    ) -> Snapshot {
        self.local_status = match providers::ping(socket, budget) {
            Ok(()) => Availability::ServiceAvailable,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Availability::TimedOut
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Availability::Incompatible,
            Err(_) => Availability::Offline,
        };
        self.snapshot()
    }

    /// Select the next provider. This is a preference, not runtime activation.
    pub fn select_provider(&mut self, provider: Provider) -> Result<Snapshot, AppError> {
        if self.dictation.busy() {
            return Err(AppError::DictationBusy);
        }
        let mut next = self.preferences.clone();
        next.selected_provider = provider;
        let data = serde_json::to_vec_pretty(&next)?;
        let dir = self.path.parent().expect("config file has a parent");
        fs::create_dir_all(dir)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // A stable lock file serializes cooperating writers across atomic renames.
        // Editors that ignore the lock are detected at this last read boundary.
        let lock = options.open(dir.join("desktop.lock"))?;
        lock.try_lock().map_err(|_| AppError::SettingsBusy)?;
        if read_optional(&self.path)? != self.original {
            return Err(AppError::ConfigConflict);
        }
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        temp.write_all(&data)?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(|e| e.error)?;
        self.preferences = next;
        self.original = Some(data);
        Ok(self.snapshot())
    }

    /// Native input integration supplies the target lease; the settings webview
    /// cannot invent a target or begin recording by selecting a preference.
    pub fn start_dictation(
        &mut self,
        target: TargetLease,
        port: &mut impl ProviderPort,
    ) -> Result<SessionKey, AppError> {
        self.start_dictation_with_context(target, DictationContext::default(), port)
    }

    pub fn start_dictation_with_context(
        &mut self,
        target: TargetLease,
        context: DictationContext,
        port: &mut impl ProviderPort,
    ) -> Result<SessionKey, AppError> {
        self.dictation
            .start(self.preferences.selected_provider, target, context, port)
    }

    pub fn stop_dictation(&mut self, port: &mut impl ProviderPort) -> Result<(), AppError> {
        self.dictation.stop(port)
    }

    pub fn cancel_dictation(&mut self, port: &mut impl ProviderPort) -> Result<(), AppError> {
        self.dictation.cancel(port)
    }

    pub fn expire_dictation(&mut self, port: &mut impl ProviderPort) -> bool {
        self.dictation.expire(port)
    }

    pub fn provider_event(
        &mut self,
        key: SessionKey,
        event: ProviderEvent,
        output: &mut impl DeliveryPort,
    ) {
        self.dictation.event(key, event, output);
    }

    pub fn retained_text(&self) -> Option<&RetainedText> {
        self.dictation.retained_text()
    }

    pub fn dismiss_retained_text(&mut self, provider: Provider, session: u64) -> bool {
        self.dictation.dismiss_retained(provider, session)
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match fs::read(path) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
