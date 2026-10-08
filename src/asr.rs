//! OpenVINO GenAI Whisper wrapper with device switching (NPU/GPU/CPU) and timing.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use openvino_genai::{WhisperGenerationConfig, WhisperPipeline};

use crate::repeat::collapse_repeats;

pub const SAMPLE_RATE: u32 = 16_000;

/// Generated tokens allowed per second of audio, on top of [`TOKENS_BASE`].
///
/// Whisper can fall into a loop, repeating one phrase until the decoder runs
/// out of room (448 positions). Each token costs ~49 ms on the NPU with
/// `medium`, so a single looping 15 s utterance took **36.7 s** in a real
/// meeting — and held up every utterance queued behind it. Real speech never
/// comes close to that: across 203 utterances from three real meetings
/// (`medium`, Japanese) the densest was **10.8 tokens/s**. 12/s + 16 cuts none
/// of them short and caps a 15 s utterance at 200 tokens, about 11 s on the
/// NPU instead of 37.
pub const TOKENS_PER_SECOND: f64 = 12.0;
pub const TOKENS_BASE: usize = 16;
/// The decoder has 448 positions and the prompt
/// (`<|startoftranscript|><|ja|><|transcribe|><|notimestamps|>`) takes 4.
const TOKENS_CEILING: usize = 444;
/// Whisper decodes audio in 30 s windows, and the cap applies to each window
/// separately, so longer audio does not earn a larger one.
const WINDOW_SECONDS: f64 = 30.0;
/// Budgets are rounded up to this, so a few dozen configs cover every length.
const BUDGET_STEP: usize = 8;

/// The `max_new_tokens` an utterance of this length is decoded with.
pub fn token_budget(audio_seconds: f64) -> usize {
    let seconds = audio_seconds.clamp(0.0, WINDOW_SECONDS);
    let budget = (seconds * TOKENS_PER_SECOND).ceil() as usize + TOKENS_BASE;
    (budget.div_ceil(BUDGET_STEP) * BUDGET_STEP).min(TOKENS_CEILING)
}

pub struct TranscribeResult {
    pub text: String,
    pub audio_seconds: f64,
    pub infer_seconds: f64,
    /// The cap this call was decoded with. `None` when the model directory has
    /// no `generation_config.json` to derive one from, so decoding ran
    /// uncapped as before.
    pub max_new_tokens: Option<usize>,
    /// Tokens the decoder produced, as OpenVINO reports them.
    pub generated_tokens: Option<usize>,
    /// Characters dropped by [`collapse_repeats`].
    pub repeats_removed: usize,
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

    /// Decoding stopped at the cap instead of at an end-of-text token, which
    /// for audio within one 30 s window means the decoder was looping.
    pub fn hit_token_cap(&self) -> bool {
        matches!(
            (self.generated_tokens, self.max_new_tokens),
            (Some(generated), Some(cap)) if generated >= cap
        )
    }
}

/// Per-length decoding configs, each with its own `max_new_tokens`.
///
/// The Rust binding offers no way to set `max_new_tokens` on a Whisper config:
/// `WhisperGenerationConfig::get_generation_config()` hands back a *copy*
/// (openvino.genai 2026.2.1 `src/c/src/whisper_generation_config.cpp`), and
/// nothing writes one back. Loading a config from JSON does read it, though,
/// so each budget is the model's own `generation_config.json` — the file the
/// pipeline itself was configured from — with that one field added.
struct TokenCaps {
    base: serde_json::Value,
    language: String,
    configs: HashMap<usize, WhisperGenerationConfig>,
}

impl TokenCaps {
    fn load(model_dir: &str, language: &str) -> Result<Self> {
        let path = Path::new(model_dir).join("generation_config.json");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("{} を読めません", path.display()))?;
        let base = serde_json::from_str(&text)
            .with_context(|| format!("{} を解釈できません", path.display()))?;
        Ok(Self {
            base,
            language: language.to_string(),
            configs: HashMap::new(),
        })
    }

    fn config(&mut self, max_new_tokens: usize) -> Result<&WhisperGenerationConfig> {
        if !self.configs.contains_key(&max_new_tokens) {
            let config = self.build(max_new_tokens)?;
            self.configs.insert(max_new_tokens, config);
        }
        Ok(&self.configs[&max_new_tokens])
    }

    fn build(&self, max_new_tokens: usize) -> Result<WhisperGenerationConfig> {
        let mut json = self.base.clone();
        json["max_new_tokens"] = max_new_tokens.into();
        // The pid keeps two instances from reading each other's half-written
        // file; the file is gone again as soon as it has been parsed.
        let path = std::env::temp_dir().join(format!(
            "meet-transcribe-generation-config-{}-{max_new_tokens}.json",
            std::process::id()
        ));
        std::fs::write(&path, serde_json::to_vec(&json)?)
            .with_context(|| format!("{} に書き込めません", path.display()))?;
        let loaded = path
            .to_str()
            .context("一時フォルダのパスが UTF-8 ではありません")
            .and_then(|p| Ok(WhisperGenerationConfig::from_json(p)?));
        let _ = std::fs::remove_file(&path);
        let mut config = loaded?;
        configure(&mut config, &self.language)?;
        Ok(config)
    }
}

fn configure(config: &mut WhisperGenerationConfig, language: &str) -> Result<()> {
    // Force Japanese + transcribe so the model doesn't spend a decode step
    // auto-detecting language on every segment.
    config.set_language(&format!("<|{language}|>"))?;
    config.set_task("transcribe")?;
    Ok(())
}

pub struct WhisperAsr {
    pipe: WhisperPipeline,
    /// The pipeline's own config, used when there is no cap to apply.
    config: WhisperGenerationConfig,
    caps: Option<TokenCaps>,
    /// Replaces [`token_budget`] when set; `bench --max-new-tokens` uses it to
    /// prove the cap reaches the device at all.
    forced_cap: Option<usize>,
    pub device: String,
}

impl WhisperAsr {
    pub fn new(model_dir: &str, device: &str, language: &str) -> Result<Self> {
        // On recent OpenVINO, device="NPU" auto-routes to the static pipeline
        // (fixed shapes + pre-allocated KV cache) needed for the NPU.
        let pipe = WhisperPipeline::new(model_dir, device)?;

        let mut config = pipe.get_generation_config()?;
        configure(&mut config, language)?;

        Ok(Self {
            pipe,
            config,
            // Every official conversion ships the file; without it decoding
            // simply runs uncapped, which is how it always ran.
            caps: TokenCaps::load(model_dir, language).ok(),
            forced_cap: None,
            device: device.to_string(),
        })
    }

    /// Decode every utterance with this cap instead of one sized to its length.
    pub fn force_max_new_tokens(&mut self, cap: Option<usize>) {
        self.forced_cap = cap;
    }

    pub fn transcribe(&mut self, audio_16k_mono: &[f32]) -> Result<TranscribeResult> {
        let seconds = audio_16k_mono.len() as f64 / SAMPLE_RATE as f64;
        let budget = self.forced_cap.unwrap_or_else(|| token_budget(seconds));
        let (config, max_new_tokens) = match self.caps.as_mut() {
            Some(caps) => (caps.config(budget)?, Some(budget)),
            None => (&self.config, None),
        };

        let t0 = Instant::now();
        let results = self.pipe.generate(audio_16k_mono, Some(config))?;
        let infer_seconds = t0.elapsed().as_secs_f64();

        let raw = results.get_string()?;
        let (text, repeats_removed) = collapse_repeats(raw.trim());
        let generated_tokens = results
            .get_perf_metrics()
            .and_then(|metrics| metrics.get_num_generation_tokens())
            .ok();
        Ok(TranscribeResult {
            text,
            audio_seconds: seconds,
            infer_seconds,
            max_new_tokens,
            generated_tokens,
            repeats_removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_grows_with_length_and_stops_at_one_window() {
        assert_eq!(token_budget(0.7), 32);
        assert_eq!(token_budget(15.0), 200);
        assert_eq!(token_budget(30.0), 376);
        assert_eq!(token_budget(60.0), token_budget(30.0));
        assert!(token_budget(1e9) <= TOKENS_CEILING);
    }

    /// The densest real utterance seen was 10.8 tokens/s. A budget that cut
    /// that short would lose real speech to save time on loops.
    #[test]
    fn budget_never_cuts_real_speech() {
        for tenth in 7..=300 {
            let seconds = tenth as f64 / 10.0;
            assert!(token_budget(seconds) as f64 >= 10.8 * seconds, "{seconds} s");
        }
    }
}
