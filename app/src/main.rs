//! meet-transcribe GUI.
//!
//! A window around the same pipeline the `live` binary runs: pick a model and a
//! device, pick which audio endpoint to listen to (system loopback or a
//! microphone), start and stop, and keep every transcript in a sidebar.
//!
//! Windows-only, like the capture layer it drives.
//!
//! The binary imports `openvino_genai_c.dll` with `/DELAYLOAD` so it can start
//! without OpenVINO on `PATH`, find the install itself, and — if it cannot —
//! open the window and say so rather than dying at load time with a bare
//! "DLL が見つかりません" dialog. See `openvino.rs`. `scripts/run_gui.ps1`
//! remains as an explicit override for an install in an unusual place.

// No console window in release; the in-app log panel is the user-facing log.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod models;
#[cfg(windows)]
mod openvino;
#[cfg(windows)]
mod store;

/// `--selftest <model-dir> <device>`: resolve OpenVINO and actually construct a
/// pipeline, then exit.
///
/// This exists because the *interesting* half of `/DELAYLOAD` cannot be checked
/// by watching the window open. The import is resolved at the first real call
/// into the DLL, which happens when a model loads — press 開始 and either it
/// works or the delay stub raises a structured exception and the process
/// disappears. A GUI binary on a machine driven over SSH has no desktop to
/// press that button on, so the binary can press it for itself.
///
/// Writes a report to `%TEMP%\meet-transcribe-selftest.txt` (release builds are
/// `windows_subsystem = "windows"` and have nowhere to print) and exits 0 or 1.
#[cfg(windows)]
fn selftest(model_dir: &str, device: &str) -> i32 {
    use std::fmt::Write as _;
    use std::path::PathBuf;

    let mut report = String::new();
    let status = openvino::prepare(None);
    let _ = writeln!(report, "openvino: {status:?}");

    let code = match meet_transcribe::asr::WhisperAsr::new(model_dir, device, "ja") {
        Ok(_) => {
            // Getting here means the delayed import resolved and a real call
            // into openvino_genai_c.dll returned.
            let _ = writeln!(report, "WhisperAsr::new({model_dir}, {device}) -> OK");
            let _ = writeln!(report, "VERDICT: delay-loaded OpenVINO import resolves at call time.");
            0
        }
        Err(e) => {
            let _ = writeln!(report, "WhisperAsr::new({model_dir}, {device}) -> ERR {e:#}");
            let _ = writeln!(report, "VERDICT: FAILED");
            1
        }
    };

    let out = PathBuf::from(std::env::var("TEMP").unwrap_or_else(|_| ".".into()))
        .join("meet-transcribe-selftest.txt");
    let _ = std::fs::write(&out, &report);
    print!("{report}");
    code
}

fn main() {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        if let Some(index) = args.iter().position(|a| a == "--selftest") {
            let model = args.get(index + 1).cloned().unwrap_or_default();
            let device = args
                .get(index + 2)
                .cloned()
                .unwrap_or_else(|| "CPU".to_string());
            std::process::exit(selftest(&model, &device));
        }
        app::run();
    }

    #[cfg(not(windows))]
    eprintln!(
        "meet-transcribe の GUI は Windows 専用です (WASAPI ループバック取り込みのため)。"
    );
}
