//! 最小的 WAV 讀取, 只給評測用 (SDD §8.1)。
//!
//! 錄音管線本身不碰檔案 —— 音訊從 cpal 直接進環形緩衝區。這裡存在的
//! 唯一理由是 `--transcribe`: 讓評測跑**產品實際的推論路徑** (shim 的
//! CTC 去重、ITN 設定、VAD 修剪), 而不是上游的 sense-voice-main CLI。
//! 兩者不必然給出同樣的文字, 拿 CLI 的分數當產品的分數是類別錯誤 ——
//! 與 M0 那次把繁化缺件算進引擎分數的錯誤同一類。
//!
//! 只支援 16-bit PCM mono, 因為 `eval/record.py` 只產生這一種。遇到
//! 其他格式直接報錯而不是猜。

use anyhow::{anyhow, bail, Result};

pub struct Wav {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

pub fn read(path: &std::path::Path) -> Result<Wav> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("{}: 不是 RIFF/WAVE 檔", path.display());
    }

    let mut sample_rate = None;
    let mut channels = 0u16;
    let mut bits = 0u16;
    let mut format = 0u16;
    let mut pos = 12;

    // 掃 chunk 而不是寫死 44 bytes 的偏移: arecord 有時會插入 LIST chunk,
    // 寫死偏移會把中繼資料當成音訊讀進來。
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;

        if id == b"fmt " {
            if body + 16 > bytes.len() {
                bail!("{}: fmt chunk 被截斷", path.display());
            }
            format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
            channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
            sample_rate = Some(u32::from_le_bytes(
                bytes[body + 4..body + 8].try_into().unwrap(),
            ));
            bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
        } else if id == b"data" {
            let rate = sample_rate.ok_or_else(|| anyhow!("data chunk 出現在 fmt 之前"))?;
            if format != 1 || bits != 16 {
                bail!(
                    "{}: 只支援 16-bit PCM (format={format}, bits={bits})",
                    path.display()
                );
            }
            if channels != 1 {
                bail!("{}: 只支援單聲道 (channels={channels})", path.display());
            }
            let end = (body + size).min(bytes.len());
            let samples = bytes[body..end]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            return Ok(Wav {
                samples,
                sample_rate: rate,
            });
        }
        // chunk 長度是奇數時後面補一個 pad byte。
        pos = body + size + (size & 1);
    }
    bail!("{}: 找不到 data chunk", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_wav(path: &std::path::Path, rate: u32, channels: u16, bits: u16, data: &[i16]) {
        let mut f = std::fs::File::create(path).unwrap();
        let data_bytes: Vec<u8> = data.iter().flat_map(|s| s.to_le_bytes()).collect();
        let block_align = channels * bits / 8;
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_bytes.len() as u32).to_le_bytes())
            .unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&rate.to_le_bytes()).unwrap();
        f.write_all(&(rate * block_align as u32).to_le_bytes()).unwrap();
        f.write_all(&block_align.to_le_bytes()).unwrap();
        f.write_all(&bits.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&(data_bytes.len() as u32).to_le_bytes()).unwrap();
        f.write_all(&data_bytes).unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("voicetype-wav-test-{name}.wav"))
    }

    #[test]
    fn reads_16bit_mono() {
        let p = tmp("mono");
        write_wav(&p, 16_000, 1, 16, &[0, 16384, -16384, 32767]);
        let w = read(&p).unwrap();
        assert_eq!(w.sample_rate, 16_000);
        assert_eq!(w.samples.len(), 4);
        assert!((w.samples[1] - 0.5).abs() < 1e-6);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_stereo() {
        let p = tmp("stereo");
        write_wav(&p, 16_000, 2, 16, &[0, 0, 0, 0]);
        assert!(read(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_non_wav() {
        let p = tmp("garbage");
        std::fs::write(&p, b"not a wav file at all").unwrap();
        assert!(read(&p).is_err());
        let _ = std::fs::remove_file(&p);
    }
}
