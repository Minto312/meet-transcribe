//! Streaming speech segmentation via Silero VAD (voice_activity_detector crate).
//!
//! Whisper is a 30-second batch model, not a true streaming model. The pragmatic
//! live approach: use VAD to detect when an utterance ends (a short silence),
//! then transcribe that whole utterance at once. This gives "finalize at end of
//! each utterance" latency -- fine for a benchmark and for reading a transcript.
//! (A lower-latency LocalAgreement-2 partial-hypothesis refinement is a
//! documented next step; see README.)

use anyhow::Result;
use voice_activity_detector::VoiceActivityDetector;

/// Silero at 16 kHz requires exactly 512-sample windows.
const WINDOW: usize = 512;

pub struct SpeechSegmenter {
    vad: VoiceActivityDetector,
    buf: Vec<f32>,      // leftover samples (< WINDOW)
    speech: Vec<f32>,   // samples of the active utterance
    in_speech: bool,
    silence_run: usize, // consecutive non-speech windows while in an utterance
    threshold: f32,
    min_silence_windows: usize,
    max_samples: usize,
    min_samples: usize,
}

impl SpeechSegmenter {
    pub fn new(threshold: f32, min_silence_ms: u32, max_segment_s: f32) -> Result<Self> {
        let vad = VoiceActivityDetector::builder()
            .sample_rate(16_000)
            .chunk_size(WINDOW)
            .build()?;
        let window_ms = (WINDOW as f32 / 16_000.0 * 1000.0).round() as u32; // ~32 ms
        Ok(Self {
            vad,
            buf: Vec::new(),
            speech: Vec::new(),
            in_speech: false,
            silence_run: 0,
            threshold,
            min_silence_windows: (min_silence_ms / window_ms).max(1) as usize,
            max_samples: (max_segment_s * 16_000.0) as usize,
            // Whisper hallucinates confident filler on near-empty input -- on
            // this machine short blips reliably produced "ご視聴ありがとうござ
            // いました". Drop anything too short to be a real utterance.
            min_samples: (0.7 * 16_000.0) as usize,
        })
    }

    /// Feed arbitrary-length 16 kHz mono audio; return any utterances that
    /// completed (ended on silence, or hit the max segment length).
    pub fn feed(&mut self, audio_16k: &[f32]) -> Vec<Vec<f32>> {
        let mut finished = Vec::new();
        self.buf.extend_from_slice(audio_16k);

        while self.buf.len() >= WINDOW {
            let win: Vec<f32> = self.buf.drain(..WINDOW).collect();
            let prob = self.vad.predict(win.clone());
            let is_speech = prob >= self.threshold;

            if is_speech {
                if !self.in_speech {
                    self.in_speech = true;
                    self.speech.clear();
                }
                self.speech.extend_from_slice(&win);
                self.silence_run = 0;
            } else if self.in_speech {
                // Keep a little trailing silence as natural padding.
                self.speech.extend_from_slice(&win);
                self.silence_run += 1;
                if self.silence_run >= self.min_silence_windows {
                    self.in_speech = false;
                    self.silence_run = 0;
                    if self.speech.len() >= self.min_samples {
                        finished.push(std::mem::take(&mut self.speech));
                    } else {
                        self.speech.clear(); // too short: almost certainly not speech
                    }
                }
            }

            // Whisper caps at 30s; force-flush over-long utterances.
            if self.in_speech && self.speech.len() >= self.max_samples {
                finished.push(std::mem::take(&mut self.speech));
                self.in_speech = false;
                self.silence_run = 0;
            }
        }
        finished
    }
}
