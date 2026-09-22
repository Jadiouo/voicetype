//! voicetyped — VoiceType 的 ASR daemon (SDD §3)。
//!
//! 與 fcitx5 addon 分成兩個行程的理由見 SDD §3.2: 崩潰隔離 (ASR segfault
//! 不能拖垮輸入法)、不阻塞 fcitx5 事件迴圈、資源可透過 systemd 限制。

mod asr;
mod assistant;
mod audio;
mod ipc;
mod personalization;
mod postproc;
mod protocol;
mod refine;
mod session;
mod vad;

use std::sync::Arc;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[cfg(not(feature = "sensevoice"))]
use crate::asr::NullTranscriber;
use crate::asr::Transcriber;
use crate::audio::{AudioCapture, StreamMode};
use crate::session::{Pipeline, SessionManager};
use crate::vad::SpeechDetector;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("VOICETYPE_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // 評測模式 (SDD §8.1)。放在 daemon 啟動之前 —— 它不開 socket、
    // 不碰麥克風, 只跑推論管線。
    if let Some(args) = TranscribeArgs::from_env()? {
        return transcribe_file(args);
    }

    info!(version = env!("CARGO_PKG_VERSION"), "voicetyped starting");

    let (asr, vad) = load_engine(true)?;
    let traditional = crate::postproc::Traditional::load().map(Arc::new);
    let vocab = Arc::new(crate::postproc::Vocab::load_or_empty(&vocab_path()));
    info!(
        engine = asr.name(),
        vad = vad.is_some(),
        traditional = traditional.is_some(),
        vocab_rules = vocab.len(),
        "asr engine"
    );

    warm_up(asr.as_ref());

    // 串流模式預設 warm (SDD §4.4)。注意這代表 GNOME 的麥克風指示燈
    // 會在閒置逾時前持續亮著 —— README 必須說明這點。
    let audio = Arc::new(AudioCapture::new(StreamMode::Warm));

    let manager = Arc::new(SessionManager::new(
        audio,
        Pipeline {
            asr,
            vad,
            traditional,
            vocab,
            assistant: Arc::new(crate::assistant::Assistant::load()?),
        },
    ));

    let path = ipc::socket_path();
    let server = ipc::Server::bind(&path)?;

    tokio::select! {
        r = server.run(manager) => r?,
        _ = tokio::signal::ctrl_c() => {
            info!("shutting down");
        }
    }
    Ok(())
}

/// `--transcribe <wav> [--no-vad] [--no-itn]`：跑一次推論並把結果印到 stdout。
///
/// 存在的理由是評測的可信度。`eval/evaluate.py` 原本呼叫上游的
/// sense-voice-main CLI, 但產品跑的是 shim —— CTC 去重、ITN 設定、
/// 現在還多了 VAD 修剪, 都可能讓兩者輸出不同的文字。拿 CLI 的分數
/// 當產品的分數是類別錯誤 (與 M0 那次把繁化缺件算進引擎分數同一類)。
///
/// `--no-vad` 讓評測能做「有無 VAD」的對照, 那是接上 VAD 的主要風險:
/// 剪過頭會吃掉第一個音節。`--no-itn` 見 `SenseVoice::load_with_itn`。
struct TranscribeArgs {
    wav: std::path::PathBuf,
    use_vad: bool,
    use_itn: bool,
    /// `--vad-report`: 印出逐窗語音機率的分布, 不做轉錄。
    ///
    /// 存在的理由: VAD 判定「有語音」時, 從輸出看不出來它是**險過**
    /// 還是**穩過**。安靜房間的底噪讓機率停在 0.5 上方一點點, 與
    /// 真的有人說話, 兩者的轉錄結果都是一段文字 —— 差別只在機率上。
    /// 調門檻之前得先看得到那個分布。
    vad_report: bool,
    /// `--repeat N`: 在**同一個行程**裡連續推論 N 次, 每次印出 RSS。
    ///
    /// 存在的理由是隔離記憶體問題。從 daemon 的 RSS 看不出來成長是
    /// 推論洩漏、mmap 頁面逐步載入, 還是配置器碎片 —— 三者都表現成
    /// 「用久了變大」。固定輸入跑 N 次可以把推論這一項單獨拉出來。
    repeat: usize,
}

impl TranscribeArgs {
    fn from_env() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1);
        let mut wav = None;
        let mut use_vad = true;
        let mut use_itn = true;
        let mut vad_report = false;
        let mut repeat = 1usize;
        let mut asked = false;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--transcribe" => {
                    asked = true;
                    wav = args.next().map(std::path::PathBuf::from);
                }
                "--no-vad" => use_vad = false,
                "--no-itn" => use_itn = false,
                "--vad-report" => vad_report = true,
                "--repeat" => {
                    repeat = args
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("--repeat 需要一個次數"))?;
                }
                other => anyhow::bail!("未知的參數: {other}"),
            }
        }
        if !asked {
            return Ok(None);
        }
        let wav = wav.ok_or_else(|| anyhow::anyhow!("--transcribe 需要一個 WAV 路徑"))?;
        Ok(Some(Self {
            wav,
            use_vad,
            use_itn,
            vad_report,
            repeat,
        }))
    }
}

fn transcribe_file(args: TranscribeArgs) -> Result<()> {
    let (transcriber, detector) = load_engine(args.use_itn)?;
    let wav = crate::audio::wav::read(&args.wav)?;
    let mut samples = crate::audio::resample::to_target_rate(&wav.samples, wav.sample_rate)?;

    if args.vad_report {
        let Some(detector) = detector.as_deref() else {
            anyhow::bail!("這個建置沒有 VAD");
        };
        return vad_report(detector, &samples);
    }

    if args.use_vad {
        if let Some(detector) = detector.as_deref() {
            match crate::vad::trim(detector, &samples, &crate::vad::VadConfig::default())? {
                Some(speech) => {
                    samples.truncate(speech.end);
                    samples.drain(..speech.start);
                }
                // 空行而不是錯誤: 評測要能區分「引擎辨識錯」與
                // 「VAD 判定沒有語音」, 前者算進 CER, 後者是空輸出。
                None => {
                    println!();
                    return Ok(());
                }
            }
        }
    }

    if args.repeat > 1 {
        return repeat_transcribe(transcriber.as_ref(), &samples, args.repeat);
    }

    let raw = transcriber.transcribe(crate::asr::Utterance {
        samples: &samples,
        language: None,
    })?;
    // 印原始輸出 (含標籤): evaluate.py 自己會剝, 而標籤是語言判定的
    // 證據, 在這裡丟掉評測就看不到「branch 被判成日文」這類錯誤。
    //
    // 繁化**不**在這裡做。evaluate.py 比對前會把兩邊都摺疊成簡體
    // (t2s), 在這裡轉一次繁再被轉回去只是白工, 而且會讓「引擎輸出」
    // 與「使用者看到的」在評測輸出裡混成一團。繁化的正確性是獨立的
    // 測試項 (postproc::traditional 的單元測試), 與 ASR 準確度分開量。
    println!("{raw}");
    Ok(())
}

/// 連續推論 N 次並印出每次的 RSS 與耗時。
fn repeat_transcribe(transcriber: &dyn Transcriber, samples: &[f32], n: usize) -> Result<()> {
    println!("{:>4}  {:>10}  {:>9}  {:>8}", "次", "RSS", "Δ", "耗時");
    let mut prev = rss_kb()?;
    let base = prev;
    for i in 1..=n {
        let t = std::time::Instant::now();
        transcriber.transcribe(crate::asr::Utterance {
            samples,
            language: None,
        })?;
        let ms = t.elapsed().as_millis();
        let rss = rss_kb()?;
        println!(
            "{i:>4}  {:>7.1} MB  {:>+6.1} MB  {ms:>5} ms",
            rss as f64 / 1024.0,
            (rss as f64 - prev as f64) / 1024.0
        );
        prev = rss;
    }
    println!(
        "\n{n} 次推論共增加 {:.1} MB (平均每次 {:.2} MB)",
        (prev as f64 - base as f64) / 1024.0,
        (prev as f64 - base as f64) / 1024.0 / n as f64
    );
    Ok(())
}

fn rss_kb() -> Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            return Ok(v.trim().trim_end_matches(" kB").trim().parse()?);
        }
    }
    anyhow::bail!("/proc/self/status 沒有 VmRSS")
}

/// 印出 VAD 的機率分布與判定結果。
fn vad_report(detector: &dyn crate::vad::SpeechDetector, samples: &[f32]) -> Result<()> {
    let cfg = crate::vad::VadConfig::default();
    let probs = detector.speech_probs(samples)?;

    let n = probs.len();
    let mut sorted = probs.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("VAD 機率不會是 NaN"));
    let pct = |p: f64| sorted[((n as f64 - 1.0) * p) as usize];

    let above = probs.iter().filter(|&&p| p >= cfg.threshold).count();
    println!(
        "{:.2}s 音訊 / {n} 窗 (每窗 {:.0}ms)",
        samples.len() as f32 / 16_000.0,
        crate::vad::HOP as f32 / 16.0
    );
    println!(
        "機率  min {:.3}  p50 {:.3}  p90 {:.3}  p99 {:.3}  max {:.3}",
        sorted[0],
        pct(0.5),
        pct(0.9),
        pct(0.99),
        sorted[n - 1]
    );
    println!(
        "超過門檻 ({:.2}) 的窗: {above}/{n} ({:.0}%)  —— 需要 ≥{} 窗才算有語音",
        cfg.threshold,
        100.0 * above as f32 / n as f32,
        (cfg.min_speech_ms as usize * 16_000 / 1000).div_ceil(crate::vad::HOP)
    );

    match crate::vad::locate_speech(&probs, samples.len(), &cfg) {
        Some(s) => println!(
            "判定: 有語音  [{:.2}s – {:.2}s] (保留 {:.0}%)",
            s.start as f32 / 16_000.0,
            s.end as f32 / 16_000.0,
            100.0 * s.len() as f32 / samples.len() as f32
        ),
        None => println!("判定: 無語音 (不會送進引擎)"),
    }

    // 逐秒的最大機率, 讓「哪一段觸發的」看得出來。
    let per_sec = 16_000 / crate::vad::HOP;
    print!("逐秒尖峰:");
    for (i, chunk) in probs.chunks(per_sec).enumerate() {
        let peak = chunk.iter().cloned().fold(0.0f32, f32::max);
        print!(" {i}s={peak:.2}");
    }
    println!();
    Ok(())
}

/// 選定並載入 ASR 引擎, 一併回傳 VAD (SDD §4.1 / §4.4)。
///
/// R1 的實測結論 (docs/r1-findings.md) 是維持 SenseVoice 單一引擎 ——
/// SDD §8.1 的 per-app 切換備案因 whisper 的延遲 (慢 16 倍) 作廢。
/// `asr::Transcriber` 這層抽象仍保留: terminal 情境的問題還沒有解,
/// 未來換引擎時不必動 session 與 IPC。
///
/// VAD 與 ASR 是同一個物件 —— Silero 的權重就在 SenseVoice 的 GGUF 裡,
/// 分開載入等於把 291MB 的模型讀兩次。
type Engine = (Arc<dyn Transcriber>, Option<Arc<dyn SpeechDetector>>);

fn load_engine(use_itn: bool) -> Result<Engine> {
    #[cfg(feature = "sensevoice")]
    {
        let path = model_path();
        let engine = crate::asr::SenseVoice::load_with_itn(&path, use_itn).map_err(|e| {
            anyhow::anyhow!("{e}\n\n模型路徑可用 VOICETYPE_MODEL 覆寫。下載方式見 README。")
        })?;
        let engine = Arc::new(engine);
        Ok((engine.clone(), Some(engine)))
    }
    #[cfg(not(feature = "sensevoice"))]
    {
        let _ = use_itn;
        // 沒有引擎的建置仍然可以驗證 IPC 與音訊管線, 但**不能默默地**
        // 當成正常 daemon 跑。
        //
        // 這個坑咬過兩次: 兩種 feature 配置產出**同一個**
        // target/release/voicetyped, 所以任何不帶 feature 的
        // `cargo build --release` **或** `cargo test --release`
        // (test 也會建 bin target) 都會把含引擎的那份覆蓋掉。
        // 之後跑起來一切正常 —— 熱鍵有反應、log 有 transcribed、
        // 游標處出現文字, 只是那個文字是 `[null asr: 3.48s]`。整條
        // 鏈路看起來都對, 唯一的破綻是 latency_ms=4 (真實推論 ~200ms)。
        //
        // 與缺 OpenCC 就中止 (eval)、安裝前綴不對就中止 (addon CMake)
        // 同一個原則: 靜默給出錯誤結果比明確失敗貴得多。
        if std::env::var("VOICETYPE_ALLOW_STUB_ENGINE").is_err() {
            anyhow::bail!(
                "這個建置不含 ASR 引擎, 不會做語音辨識。\n\n\
                 重新建置:\n  cargo build --release --features sensevoice\n\n\
                 若你**就是**要用佔位引擎測 IPC 與音訊管線, \
                 設定 VOICETYPE_ALLOW_STUB_ENGINE=1。"
            );
        }
        tracing::warn!("stub engine: 這個 daemon 不會做語音辨識");
        Ok((Arc::new(NullTranscriber), None))
    }
}

/// 啟動時跑一次丟棄的推論, 把第一次的成本挪到開機時。
///
/// 沒有這一步時, **開機後第一次**按熱鍵要等 798ms —— 實測數字, 相對
/// 之後每次的 ~190ms 是四倍, 而且正好超出 §1.3 的 P50 < 400ms 目標。
/// 成本花在把 291MB 的權重真的讀進來 (引擎不用 mmap, 但頁面要 fault in)
/// 與配置各層的計算緩衝。
///
/// 這件事在量測資源時才浮現: 剛啟動的 daemon 顯示 RSS 198MB, 看起來
/// 比預算好很多 —— 但那個數字是假的, 它只代表模型還沒真正被用起來。
/// 預熱之後 RSS 立刻是真實的 ~378MB。**寧可數字難看但誠實。**
///
/// 用 1 秒的靜音: 夠讓所有層都跑過一遍, 又不會讓啟動明顯變慢。
/// 輸出直接丟棄 (靜音會讓引擎產生幻覺文字, 那正是 VAD 存在的理由)。
fn warm_up(asr: &dyn Transcriber) {
    let t = std::time::Instant::now();
    let silence = vec![0.0f32; 16_000];
    match asr.transcribe(crate::asr::Utterance {
        samples: &silence,
        language: None,
    }) {
        Ok(_) => info!(ms = t.elapsed().as_millis() as u64, "engine warmed up"),
        // 預熱失敗不該擋住啟動 —— 真正的錄音仍然可能成功, 而且
        // 使用者寧可慢一次也不要 daemon 起不來。
        Err(e) => tracing::warn!("預熱失敗, 第一次辨識會比較慢: {e}"),
    }
}

/// 詞彙修正表的位置 (SDD §4.6 ③)。
///
/// 在 XDG config 而不是 data: 這是使用者要**編輯**的檔案, 不是程式
/// 產生的資料。install.sh 會放一份預設的過去。
fn vocab_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("VOICETYPE_VOCAB") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("voicetype").join("vocab.toml")
}

#[cfg(feature = "sensevoice")]
fn model_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("VOICETYPE_MODEL") {
        return std::path::PathBuf::from(p);
    }
    let base = std::env::var("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join(".local")
                .join("share")
        });
    base.join("voicetype")
        .join("models")
        .join("sense-voice-small-q8_0.gguf")
}
