//! Live desktop-audio transcription: WASAPI loopback -> Silero VAD -> Whisper.
//!
//!   cargo run --release --bin live -- \
//!       --model models/whisper-medium-int8-ov --device NPU
//!
//! Captures whatever your default speakers play (a Meet call, a video), segments
//! it into utterances, and transcribes each on the chosen device. Each line
//! prints its own RTF so you can watch, live, whether the NPU keeps up.
//! Windows-only (WASAPI loopback). Ctrl+C to stop.
//!
//! For a window with a model picker, a device picker and saved transcripts, see
//! the `app` package (`cargo run -p meet-transcribe-app --release`).

#[cfg(windows)]
mod imp {
    use anyhow::{bail, Result};
    use clap::Parser;
    use meet_transcribe::asr::WhisperAsr;
    use meet_transcribe::capture::{
        list_devices, AudioCapture, DeviceKind, DeviceSelection,
    };
    use meet_transcribe::vad::SpeechSegmenter;

    #[derive(Parser)]
    #[command(about = "Live desktop-audio transcription (Windows/WASAPI loopback)")]
    struct Args {
        /// OpenVINO IR model directory
        #[arg(long, required_unless_present = "list_devices")]
        model: Option<String>,
        /// NPU | GPU | CPU
        #[arg(long, default_value = "NPU")]
        device: String,
        #[arg(long, default_value = "ja")]
        language: String,
        /// Force-flush an utterance after this many seconds (Whisper caps at 30s)
        #[arg(long, default_value_t = 15.0)]
        max_segment: f32,
        /// Capture a microphone instead of the system mix
        #[arg(long)]
        mic: bool,
        /// WASAPI endpoint id to capture (see --list-devices). Defaults to the
        /// system default endpoint of the chosen kind.
        #[arg(long)]
        audio_device: Option<String>,
        /// Print the selectable audio endpoints and exit
        #[arg(long)]
        list_devices: bool,
    }

    pub fn run() -> Result<()> {
        let args = Args::parse();

        if args.list_devices {
            for device in list_devices()? {
                println!(
                    "{:<12} {}{}\n             id = {}",
                    device.kind.label(),
                    device.name,
                    if device.is_default { "  [既定]" } else { "" },
                    device.id
                );
            }
            return Ok(());
        }

        let Some(model) = args.model.as_deref() else {
            bail!("--model is required");
        };
        let selection = DeviceSelection {
            id: args.audio_device.clone(),
            kind: if args.mic {
                DeviceKind::Microphone
            } else {
                DeviceKind::Loopback
            },
        };

        // Staged progress: startup has three slow/fallible steps (NPU model
        // compile, VAD/ONNX init, WASAPI device open) and when one stalls you
        // need to know which.
        println!("[1/3] loading Whisper on {} ...", args.device);
        let mut asr = WhisperAsr::new(model, &args.device, &args.language)?;
        println!("[2/3] Whisper ready. initializing Silero VAD ...");
        let mut segmenter = SpeechSegmenter::new(0.5, 500, args.max_segment)?;
        println!("[3/3] VAD ready. opening WASAPI capture ...");
        let mut capture = AudioCapture::open(&selection)?;

        println!(
            "capturing: {} [{}] ({} Hz, {} ch)",
            capture.device_name,
            capture.kind.label(),
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
                let start = utterance.start_seconds();
                let r = asr.transcribe(&utterance.samples)?;
                if r.text.is_empty() {
                    continue;
                }
                println!(
                    "[{:02}:{:05.2}] ({:4.1}s RTF={:.2}) {}",
                    (start / 60.0) as u64,
                    start % 60.0,
                    r.audio_seconds,
                    r.rtf(),
                    r.text
                );
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
