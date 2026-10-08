//! Benchmark Whisper RTF across OpenVINO devices on THIS machine.
//!
//! There is no published RTF number for Whisper on the Core Ultra 7 155H NPU, so
//! measure it yourself. Run once idle, then AGAIN while playing a YouTube video
//! (to reproduce real desktop load), and compare how each device holds up.
//!
//!   cargo run --release --bin bench -- \
//!       --model models/whisper-large-v3-turbo-int4-ov-npu \
//!       --audio sample_ja.wav --devices NPU CPU GPU --runs 5
//!
//! RTF < 1 => faster than real time (keeps up with a live call).

use anyhow::{Context, Result};
use clap::Parser;
use meet_transcribe::asr::WhisperAsr;
use meet_transcribe::audio::{downmix_to_mono, resample_to_16k};

#[derive(Parser)]
#[command(about = "Whisper RTF benchmark across NPU/GPU/CPU")]
struct Args {
    /// OpenVINO IR model directory
    #[arg(long)]
    model: String,
    /// WAV file (any sample rate, mono or stereo)
    #[arg(long)]
    audio: String,
    /// Devices to test
    #[arg(long, num_args = 1.., default_values_t = [String::from("NPU"), String::from("CPU"), String::from("GPU")])]
    devices: Vec<String>,
    /// Timed runs per device (after a warmup run)
    #[arg(long, default_value_t = 5)]
    runs: usize,
    #[arg(long, default_value = "ja")]
    language: String,
    /// Use only the first N seconds of the audio. Real utterances are 1-15 s
    /// and fit one 30 s window; the whole sample clip does not.
    #[arg(long)]
    clip: Option<f64>,
    /// Decode with this cap instead of one sized to the audio. Set it low to
    /// prove the cap actually reaches the device: the text must come back short.
    #[arg(long)]
    max_new_tokens: Option<usize>,
}

fn load_wav_16k_mono(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {path}"))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            reader.samples::<f32>().collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Int => {
            let scale = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };

    let mono = downmix_to_mono(&samples, channels);
    Ok(resample_to_16k(&mono, spec.sample_rate))
}

fn median(mut xs: Vec<f64>) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[xs.len() / 2]
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut audio = load_wav_16k_mono(&args.audio)?;
    if let Some(clip) = args.clip {
        audio.truncate((clip * 16_000.0) as usize);
    }
    let dur = audio.len() as f64 / 16_000.0;
    println!("audio: {} ({dur:.1}s @ 16k mono)\n", args.audio);

    let mut summary: Vec<(String, Option<f64>)> = Vec::new();
    for device in &args.devices {
        println!("=== {device} ===");
        // Construction is where the device compiles the model. On the NPU this
        // dominates startup (minutes for medium), so report it separately --
        // it is a very different cost from per-utterance inference.
        let t_load = std::time::Instant::now();
        let mut asr = match WhisperAsr::new(&args.model, device, &args.language) {
            Ok(a) => a,
            Err(e) => {
                println!("  load/compile FAILED after {:.1}s: {e}\n", t_load.elapsed().as_secs_f64());
                summary.push((device.clone(), None));
                continue;
            }
        };
        println!("  load/compile: {:.1}s", t_load.elapsed().as_secs_f64());
        asr.force_max_new_tokens(args.max_new_tokens);

        // First run pays model compile + kernel JIT; report but don't count it.
        let warm = asr.transcribe(&audio)?;
        println!(
            "  warmup: {:6.2}s  RTF={:.3}  tokens={:?}/{:?}{}",
            warm.infer_seconds,
            warm.rtf(),
            warm.generated_tokens,
            warm.max_new_tokens,
            if warm.hit_token_cap() { " (hit the cap)" } else { "" }
        );

        let mut rtfs = Vec::with_capacity(args.runs);
        for i in 0..args.runs {
            let r = asr.transcribe(&audio)?;
            println!("  run {}: {:6.2}s  RTF={:.3}", i + 1, r.infer_seconds, r.rtf());
            rtfs.push(r.rtf());
        }

        let med = median(rtfs);
        let verdict = if med < 1.0 { "OK (keeps up live)" } else { "TOO SLOW for live" };
        println!("  --> median RTF={med:.3}  [{verdict}]");
        let preview: String = warm.text.chars().take(120).collect();
        println!("  text: {preview}\n");
        summary.push((device.clone(), Some(med)));
    }

    println!("=== summary (median RTF, lower is better) ===");
    for (device, med) in summary {
        match med {
            Some(m) => println!("  {device:>4}: {m:.3}  {}", if m < 1.0 { "OK" } else { "too slow" }),
            None => println!("  {device:>4}: failed to load"),
        }
    }
    Ok(())
}
