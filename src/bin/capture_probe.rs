//! Isolate WASAPI loopback capture from ASR.
//!
//! `live` stalled after "loading Whisper..." without reaching the capture
//! banner, so this probe exercises ONLY src/capture.rs: open the loopback
//! device, pull a few seconds of audio, and report signal level. If this
//! prints a non-zero RMS while something plays, capture works and the stall
//! is in the ASR half.

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use meet_transcribe::capture::LoopbackCapture;
    use std::time::{Duration, Instant};

    println!("opening WASAPI loopback ...");
    let mut cap = LoopbackCapture::open()?;
    println!(
        "opened: {} ({} Hz, {} ch)",
        cap.device_name,
        cap.sample_rate,
        cap.channels()
    );

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut chunks = 0usize;
    let mut samples = 0usize;
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;

    while Instant::now() < deadline {
        let a = cap.read_chunk()?;
        if a.is_empty() {
            continue;
        }
        chunks += 1;
        samples += a.len();
        for &v in &a {
            let m = v.abs();
            if m > peak {
                peak = m;
            }
            sum_sq += (v as f64) * (v as f64);
        }
        if chunks % 20 == 0 {
            let rms = (sum_sq / samples.max(1) as f64).sqrt();
            println!(
                "  chunks={chunks} samples={samples} ({:.1}s) peak={peak:.4} rms={rms:.5}",
                samples as f64 / 16000.0
            );
        }
    }

    let rms = (sum_sq / samples.max(1) as f64).sqrt();
    println!(
        "\ntotal: {chunks} chunks, {samples} samples ({:.1}s), peak={peak:.4}, rms={rms:.5}",
        samples as f64 / 16000.0
    );
    if peak > 0.001 {
        println!("VERDICT: real audio captured from the system mix.");
    } else if samples > 0 {
        println!("VERDICT: stream works but audio was SILENT (nothing playing, or session 0 has no endpoint).");
    } else {
        println!("VERDICT: no samples at all.");
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("capture_probe is Windows-only.");
}
