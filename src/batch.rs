//! Transcribing a backlog several utterances at a time.
//!
//! Whisper's encoder always processes a full 30 s window, so on the NPU every
//! call costs 2–2.6 s before a single token is decoded — longer than a typical
//! 1–3 s utterance takes to say. When utterances queue up (a burst of short
//! exchanges, or everything captured while the model was compiling), handing
//! them over one by one pays that fixed cost again for each. Joining what is
//! already waiting into one window pays it once.
//!
//! Only what is *already* queued is joined. An utterance that reaches an idle
//! device is transcribed on its own, straight away, so this never makes a
//! quiet meeting slower.

use std::ops::Range;

use crate::asr::SAMPLE_RATE;

/// Most audio one batch may hold, gaps included. Below Whisper's 30 s window
/// with room to spare, so a batch is always decoded in a single pass.
pub const MAX_BATCH_SECONDS: f64 = 28.0;
/// Silence put between joined utterances, so Whisper hears separate sentences
/// rather than one run-on.
pub const GAP_SECONDS: f64 = 0.3;

fn samples(seconds: f64) -> usize {
    (seconds * SAMPLE_RATE as f64) as usize
}

/// Length of a batch, gaps included, once an utterance of `next` samples is
/// added to one that is `batched` samples long (0 for an empty batch).
pub fn grown(batched: usize, next: usize) -> usize {
    let gap = if batched == 0 { 0 } else { samples(GAP_SECONDS) };
    batched + gap + next
}

/// Whether an utterance of `next` samples still fits.
pub fn fits(batched: usize, next: usize) -> bool {
    grown(batched, next) <= samples(MAX_BATCH_SECONDS)
}

/// Join utterances into one clip, a short silence between each.
pub fn join<'a>(parts: impl IntoIterator<Item = &'a [f32]>) -> Vec<f32> {
    let gap = samples(GAP_SECONDS);
    let mut out = Vec::new();
    for part in parts {
        if !out.is_empty() {
            out.resize(out.len() + gap, 0.0);
        }
        out.extend_from_slice(part);
    }
    out
}

/// How the engine would batch these utterances (lengths in samples) if all of
/// them were waiting at once: consecutive runs, each as long as fits. An
/// utterance too long to share a batch gets one to itself.
pub fn plan(lengths: &[usize]) -> Vec<Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut len = 0;
    for (i, &next) in lengths.iter().enumerate() {
        if i > start && !fits(len, next) {
            batches.push(start..i);
            start = i;
            len = 0;
        }
        len = grown(len, next);
    }
    if start < lengths.len() {
        batches.push(start..lengths.len());
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: usize = SAMPLE_RATE as usize;

    #[test]
    fn join_puts_a_gap_between_parts_only() {
        let a = vec![1.0; 3];
        let b = vec![2.0; 2];
        let joined = join([a.as_slice(), b.as_slice()]);
        assert_eq!(joined.len(), 3 + samples(GAP_SECONDS) + 2);
        assert_eq!(joined[..3], [1.0; 3]);
        assert!(joined[3..3 + samples(GAP_SECONDS)].iter().all(|&v| v == 0.0));
        assert_eq!(joined[joined.len() - 2..], [2.0; 2]);
        assert_eq!(join([a.as_slice()]), a);
    }

    #[test]
    fn plan_fills_each_batch_and_keeps_order() {
        // 10 s + 10 s fit (with one gap); a third 10 s would not.
        assert_eq!(plan(&[10 * S, 10 * S, 10 * S]), vec![0..2, 2..3]);
        // Short exchanges all fit in one.
        assert_eq!(plan(&[2 * S; 8]), vec![0..8]);
        assert_eq!(plan(&[]), Vec::<Range<usize>>::new());
    }

    #[test]
    fn an_utterance_too_long_to_share_goes_alone() {
        assert_eq!(plan(&[2 * S, 29 * S, 2 * S]), vec![0..1, 1..2, 2..3]);
    }

    #[test]
    fn a_batch_never_exceeds_the_limit() {
        let lengths: Vec<usize> = (0..200).map(|i| (1 + i % 13) * S / 2).collect();
        for range in plan(&lengths) {
            let len = lengths[range.clone()].iter().fold(0, |acc, &n| grown(acc, n));
            assert!(range.len() == 1 || len <= samples(MAX_BATCH_SECONDS), "{range:?}");
        }
    }
}
