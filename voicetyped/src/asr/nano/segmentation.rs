//! Bounded long-input routing. These durations are engineering choices, not
//! guarantees of semantic completeness; hard cuts can split spoken words.
use anyhow::{ensure, Context, Result};

use crate::vad::Speech;

pub(super) const MIN_CHUNK: usize = 3 * 16_000;
const TARGET_CHUNK: usize = 6 * 16_000;
pub(super) const MAX_CHUNK: usize = 12 * 16_000;
const MAX_AUDIO: usize = 60 * 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Boundary {
    Pause,
    Hard,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Chunk {
    pub range: Speech,
    pub speech: bool,
    pub boundary: Boundary,
}

pub(super) fn route(
    samples: &[f32],
    speech: impl FnOnce() -> Result<Vec<Speech>>,
    short: impl FnOnce(&[f32]) -> Result<String>,
    decode: impl FnMut(&[f32]) -> Result<Option<String>>,
) -> Result<String> {
    if samples.len() <= MAX_CHUNK {
        return short(samples);
    }
    transcribe(samples, &speech()?, decode)
}

pub(super) fn plan(n: usize, speech: &[Speech]) -> Result<Vec<Chunk>> {
    ensure!(
        n <= MAX_AUDIO,
        "Nano expects at most 60 seconds of 16kHz audio"
    );
    let mut previous_end = 0;
    let mut pauses = Vec::new();
    for span in speech {
        ensure!(
            span.start >= previous_end && span.start < span.end && span.end <= n,
            "invalid Nano speech span"
        );
        if previous_end < span.start {
            pauses.push(Speech {
                start: previous_end,
                end: span.start,
            });
        }
        previous_end = span.end;
    }
    if previous_end < n {
        pauses.push(Speech {
            start: previous_end,
            end: n,
        });
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < n {
        let (end, boundary) = if n - start <= TARGET_CHUNK {
            (n, Boundary::End)
        } else {
            // Keep a useful pause even in the last 6..12 seconds; merging two
            // completed short sentences can lose recorded words despite EOS.
            // Reserve at least 3 seconds for each side of a cut.
            let low = start + MIN_CHUNK;
            let high = (start + MAX_CHUNK).min(n - MIN_CHUNK);
            let target = start + TARGET_CHUNK;
            let pause = pauses
                .iter()
                .filter_map(|gap| {
                    let left = gap.start.max(low);
                    let right = gap.end.min(high);
                    if left > right {
                        return None;
                    }
                    // Clamp the midpoint within that same silence and legal
                    // sample window; choose nearest to the fixed target below.
                    Some((gap.start + (gap.end - gap.start) / 2).clamp(left, right))
                })
                .min_by_key(|&end| (end.abs_diff(target), std::cmp::Reverse(end)));
            match pause {
                Some(end) => (end, Boundary::Pause),
                None if n - start <= MAX_CHUNK => (n, Boundary::End),
                None => (high, Boundary::Hard),
            }
        };
        chunks.push(Chunk {
            range: Speech { start, end },
            speech: speech.iter().any(|s| s.start < end && s.end > start),
            boundary,
        });
        start = end;
    }
    Ok(chunks)
}

/// No output, learning or final postprocessing happens here. The caller only
/// receives text after every speech-bearing chunk passes integrity and policy.
pub(super) fn transcribe(
    samples: &[f32],
    speech: &[Speech],
    mut decode: impl FnMut(&[f32]) -> Result<Option<String>>,
) -> Result<String> {
    let chunks = plan(samples.len(), speech)?;
    let mut output = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        tracing::info!(
            chunk = index + 1, chunks = chunks.len(),
            start = chunk.range.start, end = chunk.range.end,
            speech = chunk.speech, boundary = ?chunk.boundary,
            "Nano long-input chunk"
        );
        // Pure VAD silence remains represented in the complete partition, but
        // must not enter ASR and become hallucinated text.
        if !chunk.speech {
            continue;
        }
        let text = decode(&samples[chunk.range.start..chunk.range.end])
            .with_context(|| format!("Nano chunk {} failed; entire result withheld", index + 1))?
            .filter(|text| !text.trim().is_empty())
            .with_context(|| {
                format!(
                    "Nano speech chunk {} returned no text; entire result withheld",
                    index + 1
                )
            })?;
        append_boundary(&mut output, &text);
    }
    Ok(output)
}

fn append_boundary(output: &mut String, text: &str) {
    let text = text.trim();
    // Preserve literal text and repetitions. Only keep ASCII words/numbers at
    // a chunk boundary from fusing; do not invent punctuation or deduplicate.
    if output
        .chars()
        .last()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        output.push(' ');
    }
    output.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> Speech {
        Speech { start, end }
    }
    fn check(n: usize, spans: &[Speech]) -> Vec<Chunk> {
        let chunks = plan(n, spans).unwrap();
        let mut next = 0;
        for chunk in &chunks {
            assert_eq!(chunk.range.start, next);
            assert!(chunk.range.end > next && chunk.range.end <= n);
            assert!(chunk.range.len() <= MAX_CHUNK);
            if n > MAX_CHUNK {
                assert!(chunk.range.len() >= MIN_CHUNK);
            }
            next = chunk.range.end;
        }
        assert_eq!(next, n);
        assert!(chunks.len() <= 20);
        chunks
    }

    #[test]
    fn short_route_calls_original_full_wave_once_without_new_vad_or_chunk_policy() {
        for n in [0, 1, MAX_CHUNK - 1, MAX_CHUNK] {
            let audio: Vec<_> = (0..n).map(|i| i as f32).collect();
            let mut calls = 0;
            let result = route(
                &audio,
                || panic!("new VAD on short audio"),
                |samples| {
                    calls += 1;
                    assert_eq!(samples.as_ptr(), audio.as_ptr());
                    assert_eq!(samples.len(), n);
                    Ok("  unchanged raw output  ".into())
                },
                |_| panic!("chunk policy on short audio"),
            )
            .unwrap();
            assert_eq!(result, "  unchanged raw output  ");
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn failed_long_vad_never_calls_either_decoder() {
        let audio = vec![0.2; MAX_CHUNK + 1];
        assert!(route(
            &audio,
            || anyhow::bail!("controlled VAD error"),
            |_| panic!("short decode"),
            |_| panic!("chunk decode")
        )
        .is_err());
    }

    #[test]
    fn exact_cover_thresholds_and_no_pause_hard_cuts() {
        for n in [
            0,
            1,
            MAX_CHUNK - 1,
            MAX_CHUNK,
            MAX_CHUNK + 1,
            2 * MAX_CHUNK,
            2 * MAX_CHUNK + 1,
            MAX_AUDIO,
        ] {
            let speech = if n == 0 { vec![] } else { vec![span(0, n)] };
            let chunks = check(n, &speech);
            if n <= MAX_CHUNK {
                assert_eq!(chunks.len(), usize::from(n > 0));
            }
            for chunk in chunks.iter().take(chunks.len().saturating_sub(1)) {
                assert_eq!(chunk.boundary, Boundary::Hard);
            }
        }
    }

    #[test]
    fn pause_selection_is_fixed_and_never_uses_text() {
        let spans = [
            span(0, 100_000),
            span(120_000, 155_000),
            span(165_000, 480_000),
        ];
        let chunks = check(480_000, &spans);
        assert_eq!(chunks[0].range, span(0, 110_000));
        assert_eq!(chunks[0].boundary, Boundary::Pause);
    }

    #[test]
    fn six_second_tail_ends_but_one_sample_more_can_split_at_a_pause() {
        let mid = MIN_CHUNK;
        for n in [TARGET_CHUNK, TARGET_CHUNK + 1] {
            let chunks = check(n, &[span(0, mid), span(mid + 1, n)]);
            assert_eq!(chunks.len(), if n == TARGET_CHUNK { 1 } else { 2 });
            if chunks.len() == 2 {
                assert_eq!(chunks[0].range, span(0, MIN_CHUNK));
                assert_eq!(chunks[1].range, span(MIN_CHUNK, n));
            }
        }
    }

    #[test]
    fn remaining_six_to_twelve_seconds_splits_only_when_a_pause_exists() {
        let n = 10 * 16_000;
        let midpoint = 5 * 16_000;
        let separated = check(n, &[span(0, midpoint - 100), span(midpoint + 100, n)]);
        assert_eq!(separated.len(), 2);
        assert_eq!(separated[0].range.end, midpoint);
        assert_eq!(separated[0].boundary, Boundary::Pause);
        assert_eq!(check(n, &[span(0, n)]).len(), 1);
        // A real long input leaves nine seconds after its first pause; keep
        // the second pause instead of merging the last two completed phrases.
        let n = 15 * 16_000;
        let first = 6 * 16_000;
        let second = 10 * 16_000 + 8_000;
        let chunks = check(
            n,
            &[
                span(0, first - 100),
                span(first + 100, second - 100),
                span(second + 100, n),
            ],
        );
        assert_eq!(
            chunks.iter().map(|c| c.range.end).collect::<Vec<_>>(),
            vec![first, second, n]
        );
    }

    #[test]
    fn equally_distant_pauses_choose_the_later_boundary() {
        let n = 20 * 16_000;
        let a = 5 * 16_000;
        let b = 7 * 16_000;
        let chunks = check(
            n,
            &[span(0, a - 100), span(a + 100, b - 100), span(b + 100, n)],
        );
        assert_eq!(chunks[0].range.end, b);
    }

    #[test]
    fn pause_midpoint_clamps_to_three_second_head_or_tail_limit() {
        let n = 15 * 16_000;
        let head = check(n, &[span(5 * 16_000, n)]);
        assert_eq!(head[0].range.end, MIN_CHUNK);
        let n = TARGET_CHUNK + 1;
        let tail = check(n, &[span(0, 2 * 16_000)]);
        assert_eq!(tail[0].range.end, n - MIN_CHUNK);
        assert_eq!(tail[0].boundary, Boundary::Pause);
    }

    #[test]
    fn sixty_seconds_without_pauses_uses_bounded_twelve_second_hard_cuts() {
        let chunks = check(MAX_AUDIO, &[span(0, MAX_AUDIO)]);
        assert_eq!(chunks.len(), 5);
        assert!(chunks.iter().all(|c| c.range.len() == MAX_CHUNK));
        assert!(chunks[..4].iter().all(|c| c.boundary == Boundary::Hard));
        assert_eq!(chunks[4].boundary, Boundary::End);
    }

    #[test]
    fn dense_pauses_and_fractional_tails_keep_bounded_exact_coverage() {
        for n in (MAX_CHUNK + 1..=MAX_AUDIO).step_by(509) {
            let spans: Vec<_> = (0..n)
                .step_by(8_003)
                .map(|start| span(start, (start + 4_001).min(n)))
                .collect();
            check(n, &spans);
        }
    }

    #[test]
    fn invalid_spans_and_overlength_fail_instead_of_guessing() {
        for spans in [
            vec![span(1, 1)],
            vec![span(2, 1)],
            vec![span(0, 11)],
            vec![span(0, 6), span(5, 9)],
            vec![span(5, 9), span(0, 3)],
        ] {
            assert!(plan(10, &spans).is_err());
        }
        assert!(plan(MAX_AUDIO + 1, &[]).is_err());
    }

    #[test]
    fn leading_trailing_and_entire_silence_never_reach_decoder() {
        let samples = vec![0.0; MAX_AUDIO];
        let spans = [span(20 * 16_000, 28 * 16_000)];
        let chunks = check(samples.len(), &spans);
        assert!(chunks.iter().any(|c| !c.speech));
        let mut calls = 0;
        let expected_calls = chunks.iter().filter(|c| c.speech).count();
        assert_eq!(
            transcribe(&samples, &spans, |_| {
                calls += 1;
                Ok(Some("speech".into()))
            })
            .unwrap(),
            vec!["speech"; expected_calls].join(" ")
        );
        assert_eq!(calls, expected_calls);
        assert_eq!(
            transcribe(&samples, &[], |_| panic!("silence reached ASR")).unwrap(),
            ""
        );
    }

    #[test]
    fn decoder_sees_original_ordered_samples_with_no_overlap_or_padding() {
        let samples: Vec<_> = (0..480_001).map(|i| i as f32).collect();
        let mut seen = Vec::new();
        let output = transcribe(&samples, &[span(0, samples.len())], |chunk| {
            seen.extend_from_slice(chunk);
            Ok(Some("重複".into()))
        })
        .unwrap();
        assert_eq!(seen, samples);
        assert_eq!(output, "重複重複重複");
    }

    #[test]
    fn late_failure_or_empty_speech_never_returns_earlier_text() {
        let samples = vec![0.2; 480_000];
        for failure in 0..3 {
            let mut calls = 0;
            let result = transcribe(&samples, &[span(0, samples.len())], |_| {
                calls += 1;
                if calls == 3 {
                    return match failure {
                        0 => anyhow::bail!("controlled integrity/language failure"),
                        1 => Ok(None),
                        _ => Ok(Some(" \n ".into())),
                    };
                }
                Ok(Some("must not escape".into()))
            });
            assert!(result.is_err());
            assert_eq!(calls, 3);
        }
    }

    #[test]
    fn a_late_language_rejection_uses_existing_policy_and_stops_remaining_chunks() {
        use crate::asr::{policy, Transcriber, Utterance};
        use std::sync::Mutex;
        struct Scripted(Mutex<usize>);
        impl Transcriber for Scripted {
            fn transcribe(&self, utterance: Utterance<'_>) -> Result<String> {
                assert!(utterance.language.is_none());
                let mut calls = self.0.lock().unwrap();
                *calls += 1;
                Ok(if *calls == 1 {
                    "accepted first chunk"
                } else {
                    "テスト"
                }
                .into())
            }
            fn name(&self) -> &str {
                "auto-only-scripted"
            }
            fn supports_language_hint(&self) -> bool {
                false
            }
        }
        let fake = Scripted(Mutex::new(0));
        let audio = vec![0.2; MAX_AUDIO];
        assert!(transcribe(&audio, &[span(0, audio.len())], |samples| {
            policy::transcribe(&fake, samples)
        })
        .is_err());
        assert_eq!(*fake.0.lock().unwrap(), 2);
    }

    #[test]
    fn boundary_join_preserves_repeated_words_and_punctuation() {
        let mut output = String::new();
        for text in [" hello ", "hello", "。", "中文", "中文", "GitHub", "test"] {
            append_boundary(&mut output, text);
        }
        assert_eq!(output, "hello hello。中文中文GitHub test");
    }
}
