//! Bisect an "unknown exception" from OpenVINO GenAI into an actionable cause.
//!
//! The first end-to-end run on the ThinkPad failed with a bare "unknown
//! exception" on every device, but in two distinct ways: CPU/GPU constructed
//! the pipeline and then threw inside generate(), while NPU threw during
//! construction. This binary isolates which ingredient is responsible --
//! audio length (>30 s long-form), the forced language/task config, or the
//! device itself -- by trying them one at a time and reporting each outcome.
//!
//!   cargo run --release --bin diag -- --model <dir> --audio sample_ja.wav --device CPU

use anyhow::Result;
use clap::Parser;
use hound;
use openvino_genai::WhisperPipeline;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    model: String,
    #[arg(long)]
    audio: String,
    #[arg(long, default_value = "CPU")]
    device: String,
}

fn load_wav_16k_mono(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let ch = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mono: Vec<f32> = if ch > 1 {
        samples.chunks_exact(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect()
    } else {
        samples
    };
    assert_eq!(spec.sample_rate, 16_000, "test wav must already be 16 kHz");
    Ok(mono)
}

/// Run one labelled attempt and report Ok/Err without aborting the whole probe.
fn attempt<F: FnOnce() -> Result<String>>(label: &str, f: F) {
    print!("  {label:<52} ");
    match f() {
        Ok(s) => {
            let preview: String = s.chars().take(60).collect();
            println!("OK    | {preview}");
        }
        Err(e) => println!("FAIL  | {e}"),
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let audio = load_wav_16k_mono(&args.audio)?;
    println!(
        "audio: {} samples = {:.1}s | device: {}\n",
        audio.len(),
        audio.len() as f64 / 16000.0,
        args.device
    );

    println!("--- step 1: construct pipeline ---");
    let mut pipe = match WhisperPipeline::new(&args.model, &args.device) {
        Ok(p) => {
            println!("  construct                                            OK");
            p
        }
        Err(e) => {
            println!("  construct                                            FAIL  | {e}");
            println!("\n=> the DEVICE/model pair cannot even be compiled. Stop here.");
            return Ok(());
        }
    };

    // 5 s and 20 s stay inside Whisper's single 30 s window; 37 s forces the
    // long-form chunking path, which is a prime suspect.
    let s5 = &audio[..audio.len().min(16_000 * 5)];
    let s20 = &audio[..audio.len().min(16_000 * 20)];

    println!("\n--- step 2: generate with DEFAULT config (no language/task forced) ---");
    attempt("5s, default config", || {
        Ok(pipe.generate(s5, None)?.get_string()?)
    });
    attempt("20s, default config", || {
        Ok(pipe.generate(s20, None)?.get_string()?)
    });
    attempt("full (>30s long-form), default config", || {
        Ok(pipe.generate(&audio, None)?.get_string()?)
    });

    println!("\n--- step 3: generate with language/task forced ---");
    for lang in ["<|ja|>", "ja"] {
        let mut cfg = match pipe.get_generation_config() {
            Ok(c) => c,
            Err(e) => {
                println!("  get_generation_config FAIL | {e}");
                break;
            }
        };
        let set_lang = cfg.set_language(lang);
        let set_task = cfg.set_task("transcribe");
        println!(
            "  [set_language({lang}) -> {:?}] [set_task -> {:?}]",
            set_lang.is_ok(),
            set_task.is_ok()
        );
        attempt(&format!("5s, language={lang}"), || {
            Ok(pipe.generate(s5, Some(&cfg))?.get_string()?)
        });
    }

    println!("\ndone.");
    Ok(())
}
