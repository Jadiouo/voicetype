//! cpal 音訊擷取與串流生命週期 (SDD §4.4)。
//!
//! cpal 的 `Stream` 不是 `Send`, 必須在建立它的執行緒上保持存活, 因此
//! 這裡用一條專用執行緒持有串流, 對外只透過命令通道互動。
//!
//! 音訊回呼寫入共享的環形緩衝區。回呼中取 `Mutex` 在即時音訊裡通常
//! 是禁忌 (priority inversion), 但這裡的臨界區只是一次 memcpy 到預先
//! 配置好的緩衝區, 沒有配置也沒有系統呼叫, 且唯一的競爭者是每次 PTT
//! 才出現一次的讀取端。用 lock-free ring 換來的複雜度不划算。

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use tracing::{debug, info, warn};

use super::ring::RingBuffer;
use super::{MAX_RECORDING_SECONDS, PREROLL_MS, RING_SECONDS};

/// 串流生命週期模式 (SDD §4.4)。
///
/// `Warm` 是預設值。代價是 GNOME 的麥克風使用指示燈在閒置逾時前會持續
/// 亮著 —— 這必須在 README 明確告知使用者 (SDD §7)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamMode {
    /// 首次 PTT 開啟串流, 最後一次使用後才關閉。首字延遲 0ms。
    #[default]
    Warm,
    /// 每次 PTT 才開串流, 結束立即關。首字延遲 ~80ms 且會吃掉第一個字。
    Strict,
}

/// warm 模式的閒置逾時 (SDD §4.4)。
pub const WARM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 串流開啟後的格式。取樣率由裝置決定, 重採樣延到錄音結束後批次處理。
#[derive(Debug, Clone, Copy)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// 一段錄音的起點標記。
#[derive(Debug, Clone, Copy)]
pub struct RecordingMark {
    pub position: u64,
    pub format: StreamFormat,
}

/// 取出的錄音內容 (原始取樣率、已 downmix 成 mono)。
pub struct Recording {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Recording {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

/// 共享緩衝區。音訊回呼是寫入者, session 是讀取者。
struct Shared {
    ring: Mutex<Option<ActiveRing>>,
}

struct ActiveRing {
    buffer: RingBuffer,
    format: StreamFormat,
}

enum Command {
    Ensure(mpsc::Sender<Result<StreamFormat, String>>),
    Release,
    Shutdown,
}

/// 對外的音訊擷取控制介面。
pub struct AudioCapture {
    tx: mpsc::Sender<Command>,
    shared: Arc<Shared>,
    mode: StreamMode,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioCapture {
    pub fn new(mode: StreamMode) -> Self {
        let shared = Arc::new(Shared {
            ring: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("voicetype-audio".into())
            .spawn(move || audio_thread(rx, thread_shared, mode))
            .expect("spawning audio thread");

        Self {
            tx,
            shared,
            mode,
            thread: Some(thread),
        }
    }

    pub fn mode(&self) -> StreamMode {
        self.mode
    }

    /// 確保串流已開啟, 並回傳含 pre-roll 的起點標記。
    ///
    /// 這會阻塞到串流就緒 (warm 模式下通常已經開著, 立即返回)。呼叫端
    /// 應該在 blocking 情境中使用。
    pub fn begin(&self) -> Result<RecordingMark> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .send(Command::Ensure(reply_tx))
            .map_err(|_| anyhow!("audio thread is gone"))?;
        let format = reply_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("audio thread did not respond"))?
            .map_err(|e| anyhow!(e))?;

        let preroll = (format.sample_rate as u64 * PREROLL_MS as u64 / 1000) as usize;
        let guard = self.shared.ring.lock().unwrap();
        let position = match guard.as_ref() {
            Some(active) => active.buffer.mark_with_preroll(preroll),
            None => 0,
        };
        Ok(RecordingMark { position, format })
    }

    /// 取出從 `mark` 到現在的錄音。
    pub fn end(&self, mark: RecordingMark) -> Recording {
        let samples = {
            let guard = self.shared.ring.lock().unwrap();
            match guard.as_ref() {
                Some(active) => active.buffer.take_from(mark.position),
                None => Vec::new(),
            }
        };
        if self.mode == StreamMode::Strict {
            let _ = self.tx.send(Command::Release);
        }
        Recording {
            samples,
            sample_rate: mark.format.sample_rate,
        }
    }

    /// 放棄一段錄音, 不取出資料。
    pub fn cancel(&self) {
        if self.mode == StreamMode::Strict {
            let _ = self.tx.send(Command::Release);
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn audio_thread(rx: mpsc::Receiver<Command>, shared: Arc<Shared>, mode: StreamMode) {
    // Stream 必須活在這條執行緒上。
    let mut stream: Option<cpal::Stream> = None;
    let mut idle_since: Option<std::time::Instant> = None;

    loop {
        // warm 模式下用逾時喚醒來執行閒置關閉; strict 模式沒有閒置概念,
        // 直接阻塞等命令。
        let cmd = if stream.is_some() && mode == StreamMode::Warm {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };

        match cmd {
            Some(Command::Ensure(reply)) => {
                idle_since = None;
                if let Some(active) = shared.ring.lock().unwrap().as_ref() {
                    if stream.is_some() {
                        let _ = reply.send(Ok(active.format));
                        continue;
                    }
                }
                match open_stream(&shared) {
                    Ok((s, format)) => {
                        stream = Some(s);
                        let _ = reply.send(Ok(format));
                    }
                    Err(e) => {
                        warn!("failed to open input stream: {e}");
                        let _ = reply.send(Err(e.to_string()));
                    }
                }
            }
            Some(Command::Release) => {
                close_stream(&mut stream, &shared);
                idle_since = None;
            }
            Some(Command::Shutdown) => {
                close_stream(&mut stream, &shared);
                break;
            }
            None => {
                // 逾時喚醒: 檢查 warm 模式的閒置關閉。
                match idle_since {
                    None => idle_since = Some(std::time::Instant::now()),
                    Some(t) if t.elapsed() >= WARM_IDLE_TIMEOUT => {
                        debug!("closing input stream after idle timeout");
                        close_stream(&mut stream, &shared);
                        idle_since = None;
                    }
                    Some(_) => {}
                }
            }
        }

        // 有命令進來就重置閒置計時。
        if stream.is_some() && idle_since.is_none() {
            idle_since = Some(std::time::Instant::now());
        }
    }
}

fn close_stream(stream: &mut Option<cpal::Stream>, shared: &Arc<Shared>) {
    if stream.take().is_some() {
        *shared.ring.lock().unwrap() = None;
    }
}

fn open_stream(shared: &Arc<Shared>) -> Result<(cpal::Stream, StreamFormat)> {
    // 預設 host 在 Ubuntu 上是 ALSA, 經由 pipewire-alsa 相容層接到
    // PipeWire。原生 pipewire host 需要 cpal 的 `pipewire` feature 與
    // libpipewire 開發檔; 相容層在此已經足夠且少一項依賴。
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;

    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let format = StreamFormat {
        sample_rate: config.sample_rate,
        channels: config.channels,
    };

    info!(
        sample_rate = format.sample_rate,
        channels = format.channels,
        ?sample_format,
        "opening input stream"
    );

    // 緩衝區必須容納 pre-roll 與整段錄音的重疊需求; 取 SDD 指定的 30 秒。
    {
        let mut guard = shared.ring.lock().unwrap();
        *guard = Some(ActiveRing {
            buffer: RingBuffer::with_duration(format.sample_rate, RING_SECONDS),
            format,
        });
    }

    let err_fn = |e: cpal::Error| warn!("input stream error: {e}");
    let channels = format.channels as usize;

    macro_rules! build {
        ($t:ty) => {{
            let shared = shared.clone();
            device.build_input_stream(
                config.clone(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    write_samples(&shared, data, channels);
                },
                err_fn,
                None,
            )?
        }};
    }

    let stream = match sample_format {
        SampleFormat::F32 => build!(f32),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I8 => build!(i8),
        SampleFormat::U8 => build!(u8),
        other => return Err(anyhow!("unsupported sample format: {other}")),
    };

    stream.play()?;
    Ok((stream, format))
}

/// 音訊回呼。Downmix 成 mono 後寫入環形緩衝區。
///
/// 這是即時執行緒: 不配置記憶體、不做系統呼叫、不 panic。
fn write_samples<T>(shared: &Arc<Shared>, data: &[T], channels: usize)
where
    T: cpal::Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::FromSample;

    let mut guard = match shared.ring.try_lock() {
        Ok(g) => g,
        // 讀取端正在取資料。丟掉這一塊而不是阻塞即時執行緒 ——
        // 取資料的臨界區是一次 memcpy, 實務上不會撞上。
        Err(_) => return,
    };
    let Some(active) = guard.as_mut() else {
        return;
    };

    if channels <= 1 {
        // 單聲道: 型別轉換後直接寫入。
        let mut scratch = [0.0f32; 1024];
        for chunk in data.chunks(scratch.len()) {
            for (dst, src) in scratch.iter_mut().zip(chunk.iter()) {
                *dst = f32::from_sample_(*src);
            }
            active.buffer.push(&scratch[..chunk.len()]);
        }
        return;
    }

    // 多聲道: 取各聲道平均。比只取第一聲道穩健 —— 有些裝置的第一
    // 聲道是靜音的。
    let mut scratch = [0.0f32; 1024];
    let mut n = 0;
    for frame in data.chunks_exact(channels) {
        let mut sum = 0.0f32;
        for s in frame {
            sum += f32::from_sample_(*s);
        }
        scratch[n] = sum / channels as f32;
        n += 1;
        if n == scratch.len() {
            active.buffer.push(&scratch);
            n = 0;
        }
    }
    if n > 0 {
        active.buffer.push(&scratch[..n]);
    }
}

/// 錄音長度上限檢查 (SDD §4.4)。
pub fn exceeds_max_duration(recording: &Recording) -> bool {
    recording.duration_secs() > MAX_RECORDING_SECONDS
}
