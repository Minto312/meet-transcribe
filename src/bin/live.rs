//! Live desktop-audio transcription: WASAPI loopback -> Silero VAD -> Whisper.
//!
//!   cargo run --release --bin live -- \
//!       --model models/whisper-large-v3-turbo-int4-ov-npu --device NPU
//!
//! Captures whatever your default speakers play (a Meet call, a video), segments
//! it into utterances, and transcribes each on the chosen device. Each line
//! prints its own RTF so you can watch, live, whether the NPU keeps up.
//! Windows-only (WASAPI loopback). Ctrl+C to stop.

#[cfg(windows)]
mod imp {
    use anyhow::Result;
    use clap::Parser;
    use meet_transcribe::asr::WhisperAsr;
    use meet_transcribe::capture::LoopbackCapture;
    use meet_transcribe::vad::SpeechSegmenter;

    #[derive(Parser)]
    #[command(about = "Live desktop-audio transcription (Windows/WASAPI loopback)")]
    struct Args {
        /// OpenVINO IR model directory
        #[arg(long)]
        model: String,
        /// NPU | GPU | CPU
        #[arg(long, default_value = "NPU")]
        device: String,
        #[arg(long, default_value = "ja")]
        language: String,
        /// Force-flush an utterance after this many seconds (Whisper caps at 30s)
        #[arg(long, default_value_t = 15.0)]
        max_segment: f32,
    }

    pub fn run() -> Result<()> {
        let args = Args::parse();

        // Staged progress: startup has three slow/fallible steps (NPU model
        // compile, VAD/ONNX init, WASAPI device open) and when one stalls you
        // need to know which.
        println!("[1/3] loading Whisper on {} ...", args.device);
        let mut asr = WhisperAsr::new(&args.model, &args.device, &args.language)?;
        println!("[2/3] Whisper ready. initializing Silero VAD ...");
        let mut segmenter = SpeechSegmenter::new(0.5, 500, args.max_segment)?;
        println!("[3/3] VAD ready. opening WASAPI loopback ...");
        let mut capture = LoopbackCapture::open()?;

        println!(
            "capturing: {} ({} Hz, {} ch)",
            capture.device_name,
            capture.sample_rate,
            capture.channels()
        );
        println!("listening... play audio on your speakers. Ctrl+C to stop.\n");

        loop {
            let mono_16k = capture.read_chunk()?;
            if mono_16k.is_empty() {
                continue;
            }
            for utterance in segmenter.feed(&mono_16k) {
                let r = asr.transcribe(&utterance)?;
                if r.text.is_empty() {
                    continue;
                }
                println!("({:4.1}s RTF={:.2}) {}", r.audio_seconds, r.rtf(), r.text);
            }
        }
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "`live` is Windows-only (WASAPI loopback). Build it on the ThinkPad. \
         Use `bench` on other platforms."
    );
}
