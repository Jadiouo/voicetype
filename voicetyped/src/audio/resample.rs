//! 重採樣到 SenseVoice 需要的 16kHz mono f32 (SDD §4.4)。
//!
//! 擷取端保留裝置原生取樣率 (通常 44.1k / 48k), 錄音結束後才批次重採樣。
//! 這樣音訊回呼保持極簡, 也不必在即時執行緒上維護重採樣器狀態。
//!
//! 用 sinc 插值而非「每 N 個取一個」的抽取: 沒有低通的抽取會產生
//! aliasing, 直接傷害 ASR 準確度 —— 這是省不得的地方。

use anyhow::{anyhow, Result};
use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use super::TARGET_SAMPLE_RATE;

/// 重採樣器的處理區塊大小。
const CHUNK: usize = 1024;

/// 把 mono f32 從 `from_rate` 重採樣到 16kHz。
///
/// 已經是 16kHz 時直接回傳原資料, 不做多餘的處理。
pub fn to_target_rate(samples: &[f32], from_rate: u32) -> Result<Vec<f32>> {
    if from_rate == 0 {
        return Err(anyhow!("invalid sample rate: 0"));
    }
    if from_rate == TARGET_SAMPLE_RATE {
        return Ok(samples.to_vec());
    }
    if samples.is_empty() {
        return Ok(Vec::new());
    }

    let ratio = TARGET_SAMPLE_RATE as f64 / from_rate as f64;

    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: Some(0.95),
        interpolation: SincInterpolationType::Quadratic,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = Async::<f32>::new_sinc(
        ratio,
        1.1,
        &params,
        CHUNK,
        1, // mono
        FixedAsync::Input,
    )
    .map_err(|e| anyhow!("creating resampler: {e}"))?;

    let input = SequentialSlice::new(samples, 1, samples.len())
        .map_err(|e| anyhow!("building input adapter: {e}"))?;

    // process_all 會先 reset、處理整段、並修掉起始延遲與尾端 padding。
    let out = resampler
        .process_all(&input, samples.len(), None)
        .map_err(|e| anyhow!("resampling: {e}"))?;

    Ok(out.take_data())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_already_target_rate() {
        let input = vec![0.1, 0.2, 0.3];
        let out = to_target_rate(&input, TARGET_SAMPLE_RATE).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert!(to_target_rate(&[], 48_000).unwrap().is_empty());
    }

    #[test]
    fn rejects_zero_rate() {
        assert!(to_target_rate(&[0.0], 0).is_err());
    }

    /// 48k → 16k 是 3:1。輸出長度應該接近輸入的 1/3。
    #[test]
    fn downsamples_48k_to_16k() {
        let input: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 0.01).sin() * 0.5)
            .collect();
        let out = to_target_rate(&input, 48_000).unwrap();
        let expected = 16_000f32;
        let diff = (out.len() as f32 - expected).abs();
        assert!(
            diff / expected < 0.02,
            "expected ~{expected} frames, got {}",
            out.len()
        );
    }

    #[test]
    fn downsamples_44100_to_16k() {
        let input: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let out = to_target_rate(&input, 44_100).unwrap();
        let expected = 16_000f32;
        let diff = (out.len() as f32 - expected).abs();
        assert!(
            diff / expected < 0.02,
            "expected ~{expected} frames, got {}",
            out.len()
        );
    }

    /// 重採樣後訊號必須還在 —— 抓「輸出全是靜音」這種安裝錯誤。
    /// 用 1kHz 正弦波 (遠低於 16kHz 的 Nyquist 8kHz), 振幅應該大致保留。
    #[test]
    fn preserves_signal_energy() {
        let rate = 48_000u32;
        let freq = 1000.0f32;
        let input: Vec<f32> = (0..rate)
            .map(|i| {
                (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin() * 0.5
            })
            .collect();
        let out = to_target_rate(&input, rate).unwrap();

        let rms_in = (input.iter().map(|s| s * s).sum::<f32>() / input.len() as f32).sqrt();
        let rms_out = (out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32).sqrt();
        assert!(
            (rms_out - rms_in).abs() / rms_in < 0.05,
            "RMS drifted: in={rms_in}, out={rms_out}"
        );
    }
}
