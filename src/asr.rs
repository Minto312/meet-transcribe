//! OpenVINO GenAI Whisper wrapper with device switching (NPU/GPU/CPU) and timing.

use std::time::Instant;

use anyhow::Result;
use openvino_genai::{WhisperGenerationConfig, WhisperPipeline};

pub const SAMPLE_RATE: u32 = 16_000;

pub struct TranscribeResult {
    pub text: String,
    pub audio_seconds: f64,
    pub infer_seconds: f64,
}

impl TranscribeResult {
    /// Real-Time Factor = compute time / audio duration. < 1 keeps up live.
    pub fn rtf(&self) -> f64 {
        if self.audio_seconds <= 0.0 {
            f64::NAN
        } else {
            self.infer_seconds / self.audio_seconds
        }
    }
}

pub struct WhisperAsr {
    pipe: WhisperPipeline,
    config: WhisperGenerationConfig,
    pub device: String,
}

impl WhisperAsr {
    pub fn new(model_dir: &str, device: &str, language: &str) -> Result<Self> {
        // On recent OpenVINO, device="NPU" auto-routes to the static pipeline
        // (fixed shapes + pre-allocated KV cache) needed for the NPU.
        let pipe = WhisperPipeline::new(model_dir, device)?;

        let mut config = pipe.get_generation_config()?;
        // Force Japanese + transcribe so the model doesn't spend a decode step
        // auto-detecting language on every segment.
        config.set_language(&format!("<|{language}|>"))?;
        config.set_task("transcribe")?;

        Ok(Self {
            pipe,
            config,
            device: device.to_string(),
        })
    }

    pub fn transcribe(&mut self, audio_16k_mono: &[f32]) -> Result<TranscribeResult> {
        let seconds = audio_16k_mono.len() as f64 / SAMPLE_RATE as f64;
        let t0 = Instant::now();
        let results = self.pipe.generate(audio_16k_mono, Some(&self.config))?;
        let infer_seconds = t0.elapsed().as_secs_f64();
        let text = results.get_string()?.trim().to_string();
        Ok(TranscribeResult {
            text,
            audio_seconds: seconds,
            infer_seconds,
        })
    }
}
