//! Drive `src/engine.rs` from a console, with no window in the way.
//!
//! The GUI's hard part is not the pixels, it is the threading: a model compile
//! that takes minutes, a capture thread that must stop within a second, and a
//! queue that has to drain before a run is called finished. This binary
//! exercises exactly that state machine and prints every event, so the engine
//! can be verified over SSH on a machine with no interactive desktop.
//!
//!   cargo run --release --bin engine_probe -- \
//!       --model models\whisper-small-int8-ov --device NPU --seconds 40
//!
//! Play audio through the speakers while it runs (see scripts/test_capture.ps1
//! for the same trick). Exits non-zero if the engine never reached `Running` or
//! never settled back to `Idle` after the stop.

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Result};
    use clap::Parser;
    use crossbeam_channel::RecvTimeoutError;

    use meet_transcribe::capture::{DeviceKind, DeviceSelection};
    use meet_transcribe::engine::{
        Engine, EngineCommand, EngineEvent, ModelSpec, Phase, RunConfig, DEFAULT_MAX_SEGMENT_S,
        DEFAULT_MIN_SILENCE_MS, DEFAULT_VAD_THRESHOLD,
    };

    #[derive(Parser)]
    #[command(about = "Headless exercise of the GUI engine (threading + state machine)")]
    struct Args {
        /// OpenVINO IR model directory
        #[arg(long)]
        model: PathBuf,
        /// NPU | GPU | CPU
        #[arg(long, default_value = "NPU")]
        device: String,
        #[arg(long, default_value = "ja")]
        language: String,
        /// Capture a microphone instead of the system mix
        #[arg(long)]
        mic: bool,
        /// How long to capture for, after the model is ready
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        /// Give up if the model has not compiled within this long
        #[arg(long, default_value_t = 420)]
        load_timeout: u64,
    }

    pub fn run() -> Result<()> {
        let args = Args::parse();
        let (engine, events) = Engine::spawn();

        engine.send(EngineCommand::Start(Box::new(RunConfig {
            model: ModelSpec {
                dir: args.model.clone(),
                device: args.device.clone(),
                language: args.language.clone(),
            },
            audio: DeviceSelection {
                id: None,
                kind: if args.mic {
                    DeviceKind::Microphone
                } else {
                    DeviceKind::Loopback
                },
            },
            max_segment_s: DEFAULT_MAX_SEGMENT_S,
            vad_threshold: DEFAULT_VAD_THRESHOLD,
            min_silence_ms: DEFAULT_MIN_SILENCE_MS,
        })));

        let started = Instant::now();
        let mut phase = Phase::Idle;
        let mut reached_running = false;
        let mut settled = false;
        let mut segments = 0_u32;
        let mut stop_sent_at: Option<Instant> = None;
        let mut capture_deadline: Option<Instant> = None;

        loop {
            match events.recv_timeout(Duration::from_millis(500)) {
                Ok(EngineEvent::Status(status)) => {
                    if status.phase != phase {
                        println!(
                            "[{:6.1}s] phase {:?} -> {:?}  (queued {}, dropped {}, segments {})",
                            started.elapsed().as_secs_f64(),
                            phase,
                            status.phase,
                            status.queued,
                            status.dropped,
                            status.segments
                        );
                        phase = status.phase;
                    }
                    if phase == Phase::Running && !reached_running {
                        reached_running = true;
                        capture_deadline =
                            Some(Instant::now() + Duration::from_secs(args.seconds));
                        println!(
                            "  model ready after {:.1}s; capturing for {}s -- play audio now",
                            status.load_seconds.unwrap_or_default(),
                            args.seconds
                        );
                    }
                    if phase == Phase::Idle && stop_sent_at.is_some() {
                        settled = true;
                        break;
                    }
                }
                Ok(EngineEvent::Segment(segment)) => {
                    segments += 1;
                    println!(
                        "  [{:05.1}s] ({:4.1}s RTF={:.2}) {}",
                        segment.offset_s, segment.audio_s, segment.rtf, segment.text
                    );
                }
                Ok(EngineEvent::Log(record)) => {
                    println!("  {:?}: {}", record.level, record.message);
                }
                // Far too chatty to print; it is only there to move a meter.
                Ok(EngineEvent::Level(_)) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if !reached_running
                && stop_sent_at.is_none()
                && started.elapsed() > Duration::from_secs(args.load_timeout)
            {
                bail!(
                    "model did not become ready within {}s (phase {:?})",
                    args.load_timeout,
                    phase
                );
            }
            if let Some(deadline) = capture_deadline {
                if stop_sent_at.is_none() && Instant::now() >= deadline {
                    println!("[{:6.1}s] sending Stop", started.elapsed().as_secs_f64());
                    engine.send(EngineCommand::Stop);
                    stop_sent_at = Some(Instant::now());
                }
            }
            if let Some(sent) = stop_sent_at {
                // Draining is bounded by the queue, not by wall clock; a minute
                // is generous even for a slow device with a full queue.
                if sent.elapsed() > Duration::from_secs(120) {
                    bail!("engine never settled back to Idle after Stop (phase {phase:?})");
                }
            }
        }

        engine.send(EngineCommand::Shutdown);

        println!();
        println!("reached Running : {reached_running}");
        println!("settled to Idle : {settled}");
        println!("segments        : {segments}");
        if !reached_running {
            bail!("engine never reached Running");
        }
        if !settled {
            bail!("engine never settled back to Idle");
        }
        println!("VERDICT: engine start -> capture -> stop -> drain completed cleanly.");
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("engine_probe is Windows-only (it drives WASAPI capture).");
}
