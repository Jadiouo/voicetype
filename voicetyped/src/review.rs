//! Opt-in, bounded review sampling. Disk work never runs before result delivery.
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

const SAMPLE_RATE: usize = 16_000;
const MAX_ITEMS: usize = 35;
const RETENTION: u64 = 7 * 86_400;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub daily_limit: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_limit: 5,
        }
    }
}

fn read_config(path: &Path) -> Config {
    let value = read_json::<Config>(path, 4096).unwrap_or_default();
    if (1..=5).contains(&value.daily_limit) {
        value
    } else {
        Config::default()
    }
}

#[derive(Default)]
struct Gate {
    available: AtomicBool,
    draw: AtomicU64,
}

pub struct Sample {
    pub audio: Vec<f32>,
    pub asr_text: String,
    pub output_text: String,
    /// Cancellation/new recording before completion invalidates this sample.
    pub session: u64,
    pub latest: Arc<AtomicU64>,
}

pub struct Collector {
    gate: Arc<Gate>,
    tx: mpsc::SyncSender<Sample>,
}

impl Collector {
    pub fn start() -> Option<Arc<Self>> {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let config_path = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("voicetype/review.json");
        let root = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("voicetype/review");
        // The desktop owner provides shared, absolute locations. Legacy
        // service defaults remain unchanged when no explicit path is supplied.
        let config_path = std::env::var_os("VOICETYPE_REVIEW_CONFIG")
            .map(PathBuf::from)
            .unwrap_or(config_path);
        let root = std::env::var_os("VOICETYPE_REVIEW_ROOT")
            .map(PathBuf::from)
            .unwrap_or(root);
        if !config_path.is_absolute() || !root.is_absolute() {
            return None;
        }
        Self::with_paths(config_path, root)
    }

    fn with_paths(config_path: PathBuf, root: PathBuf) -> Option<Arc<Self>> {
        let gate = Arc::new(Gate::default());
        gate.draw
            .store(now().wrapping_mul(1_000_003), Ordering::Relaxed);
        let worker_gate = gate.clone();
        let (tx, rx) = mpsc::sync_channel::<Sample>(1);
        let worker = std::thread::Builder::new()
            .name("voicetype-review".into())
            .spawn(move || {
                let mut last_cleanup = 0;
                loop {
                    let config = read_config(&config_path);
                    let timestamp = now();
                    let available = if config.enabled || root.exists() {
                        (|| -> Result<bool> {
                            let _lock = StoreLock::acquire(&root)?;
                            if timestamp.saturating_sub(last_cleanup) >= 60 {
                                prune(&root, timestamp)?;
                                last_cleanup = timestamp;
                            }
                            Ok(config.enabled
                                && quota(&root, timestamp)?.count < config.daily_limit
                                && item_paths(&root)?.len() < MAX_ITEMS)
                        })()
                        .unwrap_or(false)
                    } else {
                        false
                    };
                    worker_gate.available.store(available, Ordering::Relaxed);
                    match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(sample) => {
                            // Re-read after dequeue: disabling sampling takes precedence.
                            let config = read_config(&config_path);
                            if let Err(error) = persist(&root, &config, sample, now()) {
                                tracing::warn!("review sample storage failed: {error}");
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
        if let Err(error) = worker {
            tracing::warn!("optional review worker unavailable: {error}");
            return None;
        }
        Some(Arc::new(Self { gate, tx }))
    }

    pub fn wants(&self, seconds: f32) -> bool {
        if !(8.0..=61.0).contains(&seconds) || !self.gate.available.load(Ordering::Relaxed) {
            return false;
        }
        // SplitMix64 gives a low-cost non-security sampling draw, ~one in three.
        let mut z = self
            .gate
            .draw
            .fetch_add(0x9e3779b97f4a7c15, Ordering::Relaxed);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        (z ^ (z >> 31)) % 3 == 0
    }

    pub fn submit(&self, sample: Sample) {
        // A busy disk cannot delay speech delivery or grow an unbounded queue.
        let _ = self.tx.try_send(sample);
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({"supported": true,
            "accepting": self.gate.available.load(Ordering::Relaxed)})
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn day(timestamp: u64) -> Result<String> {
    let time = libc::time_t::try_from(timestamp)?;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // localtime_r writes to our stack value; the libc timezone is the desktop's.
    ensure!(
        !unsafe { libc::localtime_r(&time, &mut local) }.is_null(),
        "local date unavailable"
    );
    Ok(format!(
        "{:04}-{:02}-{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday
    ))
}

struct StoreLock(File);
impl StoreLock {
    fn acquire(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        ensure!(
            !fs::symlink_metadata(root)?.file_type().is_symlink(),
            "review directory is a link"
        );
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join(".lock"))?;
        // Never block the worker indefinitely behind an editor.
        ensure!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "review store busy"
        );
        Ok(Self(file))
    }
}
impl Drop for StoreLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, limit: u64) -> Result<T> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "review metadata too large");
    Ok(serde_json::from_slice(&bytes)?)
}

fn atomic_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        serde_json::to_writer(&mut file, data)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(path.parent().context("missing parent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[derive(Serialize, Deserialize)]
struct Quota {
    day: String,
    count: usize,
}
fn quota(root: &Path, timestamp: u64) -> Result<Quota> {
    let today = day(timestamp)?;
    let path = root.join(".daily.json");
    if !path.exists() {
        return Ok(Quota {
            day: today,
            count: 0,
        });
    }
    let value: Quota = read_json(&path, 4096)?;
    Ok(if value.day == today {
        value
    } else {
        Quota {
            day: today,
            count: 0,
        }
    })
}

fn valid_id(id: &str) -> bool {
    id.starts_with("r-")
        && id.len() < 100
        && id[2..].chars().all(|c| c.is_ascii_digit() || c == '-')
}

fn item_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        if item.file_type()?.is_dir() && valid_id(&item.file_name().to_string_lossy()) {
            paths.push(item.path());
        }
    }
    Ok(paths)
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    id: String,
    created_at: u64,
    duration_ms: usize,
    sample_rate: usize,
    asr_text: String,
    output_text: String,
    status: String,
    corrected_text: Option<String>,
    reviewed_at: Option<u64>,
}

fn prune(root: &Path, timestamp: u64) -> Result<()> {
    for path in item_paths(root)? {
        // Corrupt entries expire by directory age instead of living forever.
        let created = read_json::<Record>(&path.join("record.json"), 128 * 1024)
            .map(|r| r.created_at)
            .or_else(|_| {
                path.metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .map_err(anyhow::Error::from)
            })?;
        if timestamp.saturating_sub(created) >= RETENTION {
            fs::remove_dir_all(path)?;
        }
    }
    // Remove our own abandoned transaction directories from crashed writers.
    for item in fs::read_dir(root)? {
        let item = item?;
        if item.file_type()?.is_dir()
            && item
                .file_name()
                .to_string_lossy()
                .starts_with(".pending-r-")
        {
            let age = item
                .metadata()?
                .modified()?
                .elapsed()
                .unwrap_or_default()
                .as_secs();
            if age >= RETENTION {
                fs::remove_dir_all(item.path())?;
            }
        }
    }
    Ok(())
}

fn wav(file: File, audio: &[f32]) -> Result<()> {
    let mut file = BufWriter::new(file);
    let size = u32::try_from(audio.len() * 2)?;
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + size).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?;
    file.write_all(&(SAMPLE_RATE as u32).to_le_bytes())?;
    file.write_all(&(SAMPLE_RATE as u32 * 2).to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&size.to_le_bytes())?;
    for sample in audio {
        let sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        file.write_all(&((sample * 32767.0).round() as i16).to_le_bytes())?;
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    Ok(())
}

fn persist(root: &Path, config: &Config, sample: Sample, timestamp: u64) -> Result<bool> {
    if !config.enabled
        || !(1..=5).contains(&config.daily_limit)
        || sample.latest.load(Ordering::SeqCst) != sample.session
        || !(SAMPLE_RATE * 8..=SAMPLE_RATE * 61).contains(&sample.audio.len())
        || sample.asr_text.trim().is_empty()
        || sample.output_text.trim().is_empty()
        || sample.asr_text.chars().count() > 4096
        || sample.output_text.chars().count() > 4096
    {
        return Ok(false);
    }
    let _lock = StoreLock::acquire(root)?;
    prune(root, timestamp)?;
    let mut quota = quota(root, timestamp)?;
    if quota.count >= config.daily_limit || item_paths(root)?.len() >= MAX_ITEMS {
        return Ok(false);
    }
    // Reserve first so failures/restarts cannot exceed the daily cap.
    quota.count += 1;
    atomic_json(&root.join(".daily.json"), &quota)?;
    let id = format!(
        "r-{timestamp}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let temp = root.join(format!(".pending-{id}"));
    fs::create_dir(&temp)?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))?;
    let result = (|| -> Result<bool> {
        let record = Record {
            version: 1,
            id: id.clone(),
            created_at: timestamp,
            duration_ms: sample.audio.len() * 1000 / SAMPLE_RATE,
            sample_rate: SAMPLE_RATE,
            asr_text: sample.asr_text,
            output_text: sample.output_text,
            status: "pending".into(),
            corrected_text: None,
            reviewed_at: None,
        };
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temp.join("audio.wav"))?;
        wav(file, &sample.audio)?;
        atomic_json(&temp.join("record.json"), &record)?;
        if sample.latest.load(Ordering::SeqCst) != sample.session {
            return Ok(false);
        }
        fs::rename(&temp, root.join(&id))?;
        File::open(root)?.sync_all()?;
        tracing::info!("review sample retained locally");
        Ok(true)
    })();
    if temp.exists() {
        let _ = fs::remove_dir_all(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "voicetype-review-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn sample() -> Sample {
        Sample {
            audio: vec![0.0; SAMPLE_RATE * 8],
            asr_text: "sample source".into(),
            output_text: "sample output".into(),
            session: 11,
            latest: Arc::new(AtomicU64::new(11)),
        }
    }
    fn enabled() -> Config {
        Config {
            enabled: true,
            daily_limit: 5,
        }
    }

    #[test]
    fn app_settings_collector_playback_correction_and_disable_share_one_store() {
        use voicetype_app_core::review::ReviewStore;
        let home = directory();
        let config = home.join("review.json");
        let root = home.join("review");
        let ui = ReviewStore::open(config.clone(), root.clone()).unwrap();
        let initial = ui.settings().unwrap();
        assert!(!initial.enabled);
        let enabled = ui.set_enabled(&initial.revision, true).unwrap();
        let collector = Collector::with_paths(config, root.clone()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while !collector.gate.available.load(Ordering::Relaxed) {
            assert!(
                std::time::Instant::now() < deadline,
                "collector ignored UI setting"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut value = sample();
        value.output_text = "請 push 到 geeho。".into();
        collector.submit(value);
        let item = loop {
            if let Ok(mut items) = ui.list(now()) {
                if let Some(item) = items.pop() {
                    break item;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "sample not available to UI"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(item.duration_ms, 8000);
        assert_eq!(
            ui.audio(&item.id, &item.revision, now()).unwrap().len(),
            44 + 8000 * 32
        );
        let corrected = ui
            .review(
                &item.id,
                &item.revision,
                "請 push 到 GitHub。".into(),
                now(),
            )
            .unwrap();
        assert_eq!(corrected.status, "corrected");
        ui.set_enabled(&enabled.revision, false).unwrap();
        collector.submit(sample());
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while collector.gate.available.load(Ordering::Relaxed) {
            assert!(
                std::time::Instant::now() < deadline,
                "collector ignored disabling"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // Existing reviewed audio remains available until explicit deletion/expiry.
        assert_eq!(ui.list(now()).unwrap().len(), 1);
        drop(collector);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn wav_preserves_order_length_and_silent_pauses() {
        let root = directory();
        let mut value = sample();
        value.audio[0] = -1.0;
        value.audio[1] = -0.5;
        value.audio[80_000] = 0.5;
        *value.audio.last_mut().unwrap() = 1.0;
        assert!(persist(&root, &enabled(), value, now()).unwrap());
        let paths = item_paths(&root).unwrap();
        let data = fs::read(paths[0].join("audio.wav")).unwrap();
        assert_eq!(&data[..4], b"RIFF");
        assert_eq!(&data[8..16], b"WAVEfmt ");
        assert_eq!(data.len(), 44 + SAMPLE_RATE * 8 * 2);
        let pcm: Vec<_> = data[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(pcm[0], -32767);
        assert_eq!(pcm[1], -16384);
        assert!(pcm[2..80_000].iter().all(|x| *x == 0));
        assert_eq!(pcm[80_000], 16384);
        assert_eq!(*pcm.last().unwrap(), 32767);
        assert_eq!(root.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        for name in ["audio.wav", "record.json"] {
            assert_eq!(
                paths[0].join(name).metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disabled_short_empty_and_stale_samples_are_not_persisted() {
        let root = directory();
        assert!(!persist(&root, &Config::default(), sample(), now()).unwrap());
        let mut value = sample();
        value.audio.truncate(SAMPLE_RATE * 7);
        assert!(!persist(&root, &enabled(), value, now()).unwrap());
        let mut value = sample();
        value.output_text.clear();
        assert!(!persist(&root, &enabled(), value, now()).unwrap());
        let value = sample();
        value.latest.store(12, Ordering::SeqCst);
        assert!(!persist(&root, &enabled(), value, now()).unwrap());
        let value = sample();
        value.latest.store(0, Ordering::SeqCst);
        assert!(!persist(&root, &enabled(), value, now()).unwrap());
        assert!(item_paths(&root).unwrap().is_empty());
        assert!(!root.join(".daily.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quota_survives_restart_and_deleting_samples_and_resets_next_day() {
        let root = directory();
        let timestamp = now();
        for _ in 0..5 {
            assert!(persist(&root, &enabled(), sample(), timestamp).unwrap());
        }
        assert!(!persist(&root, &enabled(), sample(), timestamp).unwrap());
        for path in item_paths(&root).unwrap() {
            fs::remove_dir_all(path).unwrap();
        }
        assert!(!persist(&root, &enabled(), sample(), timestamp).unwrap());
        assert!(persist(&root, &enabled(), sample(), timestamp + 86_400).unwrap());
        assert_eq!(quota(&root, timestamp + 86_400).unwrap().count, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn seven_day_cleanup_includes_reviewed_records() {
        let root = directory();
        let timestamp = now() - RETENTION + 10;
        persist(&root, &enabled(), sample(), timestamp).unwrap();
        let path = item_paths(&root).unwrap().pop().unwrap();
        let mut record: Record = read_json(&path.join("record.json"), 128 * 1024).unwrap();
        record.status = "correct".into();
        atomic_json(&path.join("record.json"), &record).unwrap();
        prune(&root, now()).unwrap();
        assert!(path.exists());
        prune(&root, now() + 11).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn full_store_and_busy_lock_fail_without_waiting_or_eviction() {
        let root = directory();
        for i in 0..MAX_ITEMS {
            let path = root.join(format!("r-{}-1-{i}", now()));
            fs::create_dir(path).unwrap();
        }
        assert!(!persist(&root, &enabled(), sample(), now()).unwrap());
        assert_eq!(item_paths(&root).unwrap().len(), MAX_ITEMS);
        let lock = StoreLock::acquire(&root).unwrap();
        assert!(persist(&root, &enabled(), sample(), now()).is_err());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_quota_fails_closed_and_does_not_write_audio() {
        let root = directory();
        fs::write(root.join(".daily.json"), b"broken").unwrap();
        assert!(persist(&root, &enabled(), sample(), now()).is_err());
        assert!(item_paths(&root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sampler_does_no_io_and_full_channel_drops_immediately() {
        let gate = Arc::new(Gate::default());
        let (tx, rx) = mpsc::sync_channel(1);
        let collector = Collector {
            gate: gate.clone(),
            tx,
        };
        assert!(!collector.wants(12.0));
        gate.available.store(true, Ordering::Relaxed);
        assert!(!collector.wants(7.9));
        assert!(!collector.wants(62.0));
        let accepted = (0..300).filter(|_| collector.wants(12.0)).count();
        assert!((60..150).contains(&accepted));
        collector.submit(sample());
        collector.submit(sample());
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn background_worker_hot_reloads_opt_in_and_writes_real_queue_items() {
        let base = directory();
        let root = base.join("samples");
        let config = base.join("settings.json");
        let collector = Collector::with_paths(config.clone(), root.clone()).unwrap();
        let wait = |predicate: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !predicate() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(predicate());
        };
        assert!(!collector.wants(10.0));
        atomic_json(&config, &enabled()).unwrap();
        wait(&|| collector.gate.available.load(Ordering::Relaxed));
        collector.submit(sample());
        wait(&|| item_paths(&root).map(|p| p.len() == 1).unwrap_or(false));
        let record: Record = read_json(
            &item_paths(&root).unwrap()[0].join("record.json"),
            128 * 1024,
        )
        .unwrap();
        assert_eq!(record.output_text, "sample output");
        atomic_json(&config, &Config::default()).unwrap();
        wait(&|| !collector.gate.available.load(Ordering::Relaxed));
        collector.submit(sample());
        drop(collector);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(item_paths(&root).unwrap().len(), 1);
        fs::remove_dir_all(base).unwrap();
    }
}
