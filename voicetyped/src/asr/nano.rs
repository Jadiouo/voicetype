//! Optional, single-resident Nano recognizer and independent Silero gate.
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::sync::Mutex;

use super::{Transcriber, Utterance};
use crate::vad::{Speech, SpeechGate};
use anyhow::{anyhow, ensure, Context, Result};

mod segmentation;

#[repr(C)]
struct VtNano {
    _private: [u8; 0],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NativeSpeechSpan {
    start: usize,
    end: usize,
}
extern "C" {
    fn vt_nano_init(
        directory: *const c_char,
        vad: *const c_char,
        use_itn: c_int,
        error: *mut c_char,
        error_cap: usize,
    ) -> *mut VtNano;
    fn vt_nano_free(ctx: *mut VtNano);
    fn vt_nano_transcribe(
        ctx: *mut VtNano,
        samples: *const f32,
        count: usize,
        out: *mut c_char,
        out_cap: usize,
        error: *mut c_char,
        error_cap: usize,
    ) -> c_int;
    fn vt_nano_has_speech(
        ctx: *mut VtNano,
        samples: *const f32,
        count: usize,
        error: *mut c_char,
        error_cap: usize,
    ) -> c_int;
    fn vt_nano_speech_spans(
        ctx: *mut VtNano,
        samples: *const f32,
        count: usize,
        spans: *mut NativeSpeechSpan,
        spans_cap: usize,
        error: *mut c_char,
        error_cap: usize,
    ) -> c_int;
}

struct Handle(*mut VtNano);
// SAFETY: access is serialized by Nano::handle; native contexts have no TLS ownership.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { vt_nano_free(self.0) };
    }
}

pub struct Nano {
    handle: Mutex<Handle>,
}

fn native_error(error: &[c_char]) -> anyhow::Error {
    // The shim always writes a bounded NUL-terminated error; buffer starts zeroed.
    anyhow!(
        "Nano: {}",
        unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy()
    )
}

impl Nano {
    pub fn load(directory: &Path, vad: &Path, use_itn: bool) -> Result<Self> {
        for relative in [
            "encoder_adaptor.int8.onnx",
            "embedding.int8.onnx",
            "llm.int8.onnx",
            "Qwen3-0.6B/tokenizer.json",
            "Qwen3-0.6B/vocab.json",
            "Qwen3-0.6B/merges.txt",
        ] {
            ensure!(
                directory.join(relative).is_file(),
                "missing Nano asset: {}",
                directory.join(relative).display()
            );
        }
        ensure!(
            vad.is_file(),
            "missing independent Silero model: {}",
            vad.display()
        );
        let directory = CString::new(directory.as_os_str().as_encoded_bytes())?;
        let vad = CString::new(vad.as_os_str().as_encoded_bytes())?;
        let mut error = [0 as c_char; 1024];
        let start = std::time::Instant::now();
        let pointer = unsafe {
            vt_nano_init(
                directory.as_ptr(),
                vad.as_ptr(),
                use_itn as c_int,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(!pointer.is_null(), "{}", native_error(&error));
        tracing::info!(
            load_ms = start.elapsed().as_millis() as u64,
            "Nano model loaded; first real inference has not been warmed"
        );
        Ok(Self {
            handle: Mutex::new(Handle(pointer)),
        })
    }

    pub fn from_env(use_itn: bool) -> Result<Self> {
        let directory = std::env::var_os("VOICETYPE_NANO_MODEL_DIR")
            .context("nano profile requires VOICETYPE_NANO_MODEL_DIR")?;
        let vad = std::env::var_os("VOICETYPE_NANO_VAD_MODEL")
            .context("nano profile requires VOICETYPE_NANO_VAD_MODEL")?;
        Self::load(Path::new(&directory), Path::new(&vad), use_itn)
    }

    fn speech_spans(&self, samples: &[f32]) -> Result<Vec<Speech>> {
        let guard = self
            .handle
            .lock()
            .map_err(|_| anyhow!("Nano context poisoned"))?;
        let mut spans = [NativeSpeechSpan::default(); 512];
        let mut error = [0 as c_char; 1024];
        let written = unsafe {
            vt_nano_speech_spans(
                guard.0,
                samples.as_ptr(),
                samples.len(),
                spans.as_mut_ptr(),
                spans.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(written >= 0, "{}", native_error(&error));
        ensure!(
            (written as usize) <= spans.len(),
            "invalid Nano speech span count"
        );
        Ok(spans[..written as usize]
            .iter()
            .map(|s| Speech {
                start: s.start,
                end: s.end,
            })
            .collect())
    }

    fn transcribe_once(&self, samples: &[f32]) -> Result<String> {
        let mut output = vec![0u8; 64 * 1024];
        let mut error = [0 as c_char; 1024];
        let guard = self
            .handle
            .lock()
            .map_err(|_| anyhow!("Nano context poisoned"))?;
        let written = unsafe {
            vt_nano_transcribe(
                guard.0,
                samples.as_ptr(),
                samples.len(),
                output.as_mut_ptr().cast(),
                output.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ensure!(written >= 0, "{}", native_error(&error));
        ensure!(
            (written as usize) < output.len(),
            "invalid Nano output length"
        );
        output.truncate(written as usize);
        String::from_utf8(output).context("Nano returned invalid UTF-8; no text sent")
    }
}

// Apply the same existing language policy independently to each native stream,
// without recursively entering the long-input router or postprocessing text.
struct NativeChunk<'a>(&'a Nano);
impl Transcriber for NativeChunk<'_> {
    fn transcribe(&self, utterance: Utterance<'_>) -> Result<String> {
        ensure!(
            utterance.language.is_none(),
            "Nano cannot honor a language retry"
        );
        self.0.transcribe_once(utterance.samples)
    }
    fn name(&self) -> &str {
        "funasr-nano:chunk"
    }
    fn supports_language_hint(&self) -> bool {
        false
    }
}

impl Transcriber for Nano {
    fn transcribe(&self, utterance: Utterance<'_>) -> Result<String> {
        ensure!(
            utterance.language.is_none(),
            "Nano 1.13.8 cannot honor per-utterance language retry; no text sent"
        );
        segmentation::route(
            utterance.samples,
            || self.speech_spans(utterance.samples),
            |samples| self.transcribe_once(samples),
            |chunk| super::policy::transcribe(&NativeChunk(self), chunk),
        )
    }
    fn name(&self) -> &str {
        "funasr-nano:int8:cpu"
    }
    fn supports_language_hint(&self) -> bool {
        false
    }
    fn warmup_with_silence(&self) -> bool {
        false
    }
}

impl SpeechGate for Nano {
    fn has_speech(&self, samples: &[f32]) -> Result<bool> {
        let guard = self
            .handle
            .lock()
            .map_err(|_| anyhow!("Nano context poisoned"))?;
        let mut error = [0 as c_char; 1024];
        let result = unsafe {
            vt_nano_has_speech(
                guard.0,
                samples.as_ptr(),
                samples.len(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        match result {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(native_error(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_assets_fail_before_native_model_creation() {
        let result = Nano::load(
            Path::new("/nonexistent/voicetype-nano"),
            Path::new("/nonexistent/vad"),
            true,
        );
        assert!(result.is_err());
    }

    /// Explicit local artifact validation. Never run as a normal unit test.
    #[test]
    #[ignore = "requires pinned models and explicit audio-only gate manifest"]
    fn native_gate_manifest() -> Result<()> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Case {
            id: String,
            wav: std::path::PathBuf,
            expected_speech: bool,
        }
        let path = std::env::var("VOICETYPE_NANO_GATE_MANIFEST")?;
        let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(!cases.is_empty(), "empty gate manifest");
        let engine = Nano::from_env(true)?;
        for case in cases {
            let wav = crate::audio::wav::read(&case.wav)?;
            let samples = crate::audio::resample::to_target_rate(&wav.samples, wav.sample_rate)?;
            let speech = engine.has_speech(&samples)?;
            println!(
                "gate {} speech={} samples={}",
                case.id,
                speech,
                samples.len()
            );
            ensure!(
                speech == case.expected_speech,
                "gate mismatch for {}",
                case.id
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires pinned models and explicit audio-only probe manifest"]
    fn native_probe_manifest() -> Result<()> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Case {
            id: String,
            wav: std::path::PathBuf,
        }
        let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(std::env::var(
            "VOICETYPE_NANO_PROBE_MANIFEST",
        )?)?)?;
        ensure!(!cases.is_empty(), "empty probe manifest");
        let load_started = std::time::Instant::now();
        let engine = std::sync::Arc::new(Nano::from_env(true)?);
        println!(
            "nano_ready {}",
            serde_json::json!({"seconds":load_started.elapsed().as_secs_f64()})
        );
        let preparation = crate::vad::AudioPreparation::FullWaveformGate(engine.clone());
        for case in cases {
            let wav = crate::audio::wav::read(&case.wav)?;
            let samples = crate::audio::resample::to_target_rate(&wav.samples, wav.sample_rate)?;
            let range = preparation
                .prepare(&samples)?
                .context("speech gate rejected probe")?;
            ensure!(
                range.start == 0 && range.end == samples.len(),
                "gate changed full-waveform contract"
            );
            for repetition in 1..=4 {
                let started = std::time::Instant::now();
                let text = engine.transcribe(Utterance {
                    samples: &samples[range.start..range.end],
                    language: None,
                })?;
                println!(
                    "nano_probe {}",
                    serde_json::json!({"id":case.id,"repetition":repetition,
                    "text":text,"seconds":started.elapsed().as_secs_f64(),"samples":samples.len(),
                    "range_start":range.start,"range_end":range.end})
                );
            }
        }
        Ok(())
    }

    /// Frozen private corpus acceptance: one decode, production policy/output,
    /// no references, no feedback, no context helper and no microphone.
    #[test]
    #[ignore = "requires isolated user-rule copies and explicit audio-only corpus manifest"]
    fn native_corpus_manifest() -> Result<()> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("info")
            .with_writer(std::io::stderr)
            .try_init();
        use crate::assistant::Assistant;
        use crate::personalization::{ContextSnapshot, Personalization};
        use crate::postproc::{Traditional, Vocab};
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Case {
            id: String,
            wav: std::path::PathBuf,
            program: String,
            context_id: String,
        }
        let cases: Vec<Case> = serde_json::from_slice(&std::fs::read(std::env::var(
            "VOICETYPE_NANO_CORPUS_MANIFEST",
        )?)?)?;
        ensure!(!cases.is_empty(), "empty corpus manifest");
        let learning =
            std::path::PathBuf::from(std::env::var("VOICETYPE_LEARNING_FILE")?).canonicalize()?;
        ensure!(
            learning != Personalization::default_path()?.canonicalize()?,
            "corpus acceptance requires a private learning copy"
        );
        let assistant = Assistant::new(Personalization::load(&learning)?, None);
        let original_rules = assistant.list()?;
        let traditional = Traditional::load().context("OpenCC required for corpus acceptance")?;
        let vocab = Vocab::load(&crate::vocab_path())?;
        let started = std::time::Instant::now();
        let engine = std::sync::Arc::new(Nano::from_env(true)?);
        let preparation = crate::vad::AudioPreparation::FullWaveformGate(engine.clone());
        println!(
            "nano_ready {}",
            serde_json::json!({"seconds":started.elapsed().as_secs_f64()})
        );
        for case in cases {
            let started = std::time::Instant::now();
            let mut stage = "audio";
            let mut raw = String::new();
            let mut sample_count = 0;
            let mut decode_seconds = None;
            let result = (|| -> Result<String> {
                let wav = crate::audio::wav::read(&case.wav)?;
                let samples =
                    crate::audio::resample::to_target_rate(&wav.samples, wav.sample_rate)?;
                sample_count = samples.len();
                stage = "speech_gate";
                let range = preparation.prepare_live(&samples)?.context("no_speech")?;
                ensure!(
                    range.start == 0 && range.end == samples.len(),
                    "full-waveform gate changed audio"
                );
                stage = "asr_language_policy";
                let decode_started = std::time::Instant::now();
                let decoded = crate::asr::policy::transcribe(engine.as_ref(), &samples);
                decode_seconds = Some(decode_started.elapsed().as_secs_f64());
                raw = decoded?.context("no_speech")?;
                ensure!(!raw.trim().is_empty(), "empty_result");
                stage = "output";
                let scope = ContextSnapshot {
                    program: case.program.clone(),
                    context_id: case.context_id.clone(),
                    ..Default::default()
                };
                let text = crate::output::process(
                    Some(&traditional),
                    &vocab,
                    &assistant,
                    &raw,
                    &scope,
                    Some("off"),
                )?;
                ensure!(!text.trim().is_empty(), "empty_result");
                Ok(text)
            })();
            let (text, error) = match result {
                Ok(text) => (text, None),
                Err(error) => (String::new(), Some(format!("{error:#}"))),
            };
            println!(
                "nano_case {}",
                serde_json::json!({"id":case.id,
                "text":text,"asr_text":raw,"error":error,"stage":stage,
                "samples":sample_count,"decode_seconds":decode_seconds,
                "seconds":started.elapsed().as_secs_f64()})
            );
        }
        ensure!(assistant.list()? == original_rules, "corpus modified rules");
        Ok(())
    }
}
