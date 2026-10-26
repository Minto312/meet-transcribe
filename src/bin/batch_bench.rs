//! What joining queued utterances buys, measured on THIS machine.
//!
//! Splits a WAV into utterances with the Silero VAD settings the live path uses,
//! then transcribes them on one compiled pipeline in two ways: one call per
//! utterance, the way a backlog used to drain, and joined into batches by
//! `batch::plan`, the way the engine drains one now. Both timings and both
//! transcripts are printed, so the saving and any change in the text sit side
//! by side.
//!
//!   cargo run --release --bin batch_bench -- \
//!       --model models\whisper-medium-int8-ov --audio sample_ja.wav --device NPU

use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Parser;
use meet_transcribe::asr::{WhisperAsr, SAMPLE_RATE};
use meet_transcribe::audio::{downmix_to_mono, resample_to_16k};
use meet_transcribe::batch;
use meet_transcribe::vad::SpeechSegmenter;

#[derive(Parser)]
#[command(about = "Per-utterance vs batched transcription of one WAV")]
struct Args {
    /// OpenVINO IR model directory
    #[arg(long)]
    model: String,
    /// WAV file (any sample rate, mono or stereo)
    #[arg(long)]
    audio: String,
    #[arg(long, default_value = "NPU")]
    device: String,
    #[arg(long, default_value = "ja")]
    language: String,
    /// Times to run both modes; transcripts are printed for the first only.
    #[arg(long, default_value_t = 2)]
    rounds: usize,
}

fn load_wav_16k_mono(path: &str) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("open {path}"))?;
    let spec = reader.spec();
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
    let mono = downmix_to_mono(&samples, spec.channels as usize);
    Ok(resample_to_16k(&mono, spec.sample_rate))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let audio = load_wav_16k_mono(&args.audio)?;

    // The live defaults: threshold 0.5, 500 ms of silence ends an utterance,
    // 15 s forces one out.
    let mut segmenter = SpeechSegmenter::new(0.5, 500, 15.0)?;
    let mut utterances: Vec<Vec<f32>> =
        segmenter.feed(&audio).into_iter().map(|u| u.samples).collect();
    // An utterance only closes on silence; give the last one some.
    let tail = vec![0.0; SAMPLE_RATE as usize];
    utterances.extend(segmenter.feed(&tail).into_iter().map(|u| u.samples));
    if utterances.len() < 2 {
        bail!("{} の発話が {} 個しかなく、まとめる対象がありません", args.audio, utterances.len());
    }

    let lengths: Vec<usize> = utterances.iter().map(Vec::len).collect();
    let plan = batch::plan(&lengths);
    let speech: usize = lengths.iter().sum();
    println!(
        "audio: {} -> {} utterances, {:.1}s of speech -> {} batches",
        args.audio,
        utterances.len(),
        speech as f64 / SAMPLE_RATE as f64,
        plan.len()
    );

    let t_load = Instant::now();
    let mut asr = WhisperAsr::new(&args.model, &args.device, &args.language)?;
    println!("load/compile on {}: {:.1}s", args.device, t_load.elapsed().as_secs_f64());
    // Neither mode should pay the first call's warm-up.
    asr.transcribe(&utterances[0])?;

    for round in 1..=args.rounds {
        let verbose = round == 1;

        if verbose {
            println!("\n=== one call per utterance ===");
        }
        let mut single = 0.0;
        for (i, utterance) in utterances.iter().enumerate() {
            let r = asr.transcribe(utterance)?;
            single += r.infer_seconds;
            if verbose {
                println!("  #{i:<2} {:5.1}s audio {:6.2}s  {}", r.audio_seconds, r.infer_seconds, r.text);
            }
        }

        if verbose {
            println!("\n=== batched ===");
        }
        let mut batched = 0.0;
        for range in &plan {
            let joined = batch::join(utterances[range.clone()].iter().map(Vec::as_slice));
            let r = asr.transcribe(&joined)?;
            batched += r.infer_seconds;
            if verbose {
                println!(
                    "  #{}-#{} {:5.1}s audio {:6.2}s  {}",
                    range.start,
                    range.end - 1,
                    r.audio_seconds,
                    r.infer_seconds,
                    r.text
                );
            }
        }

        println!(
            "\nround {round}: one call each {single:.2}s ({} calls), batched {batched:.2}s ({} calls) -> {:.0}% less",
            utterances.len(),
            plan.len(),
            100.0 * (1.0 - batched / single)
        );
    }
    Ok(())
}
