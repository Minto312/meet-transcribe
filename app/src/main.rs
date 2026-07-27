//! meet-transcribe GUI.
//!
//! A window around the same pipeline the `live` binary runs: pick a model and a
//! device, pick which audio endpoint to listen to (system loopback or a
//! microphone), start and stop, and keep every transcript in a sidebar. The
//! OpenVINO runtime and the models are downloaded by the app itself on first run
//! (`setup.rs`), and the app updates itself from its GitHub releases
//! (`update.rs`).
//!
//! Windows-only, like the capture layer it drives.
//!
//! The binary imports `openvino_genai_c.dll` with `/DELAYLOAD` so it can start
//! without OpenVINO on `PATH`, find the install itself, and — if it cannot —
//! open the window and say so rather than dying at load time with a bare
//! "DLL が見つかりません" dialog. See `openvino.rs`. `scripts/run_gui.ps1`
//! remains as an explicit override for an install in an unusual place.
//!
//! Three console modes exist because the reference machine is driven over SSH
//! and has no interactive desktop, so the halves of this app that a screenshot
//! cannot check press their own buttons: `--selftest`, `--setup` and
//! `--update-check`.

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
mod setup;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod update;

/// `--selftest <model-dir> <device> [openvino-root]`: resolve OpenVINO and
/// actually construct a pipeline, then exit.
///
/// This exists because the *interesting* half of `/DELAYLOAD` cannot be checked
/// by watching the window open. The import is resolved at the first real call
/// into the DLL, which happens when a model loads — press 開始 and either it
/// works or the delay stub raises a structured exception and the process
/// disappears. A GUI binary on a machine driven over SSH has no desktop to
/// press that button on, so the binary can press it for itself.
///
/// The optional third argument pins which install to use, which is how the
/// runtime the setup panel downloaded gets tested rather than whichever one the
/// search happens to find first on a development machine.
///
/// Writes a report to `%TEMP%\meet-transcribe-selftest.txt` (release builds are
/// `windows_subsystem = "windows"` and have nowhere to print) and exits 0 or 1.
#[cfg(windows)]
fn selftest(model_dir: &str, device: &str, openvino_root: Option<&str>) -> i32 {
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};

    let mut report = String::new();
    let status = openvino::prepare(openvino_root.map(Path::new), None);
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

/// `--update-check [--endpoint URL] [--force] [--apply]`
///
/// Hand-rolled rather than clap: the GUI package has no CLI to speak of, and a
/// flag parser that only ever sees these three options is shorter than the
/// derive it would replace.
#[cfg(windows)]
fn update_options(args: &[String]) -> update::ProbeOptions {
    let position = |flag: &str| args.iter().position(|a| a == flag);
    update::ProbeOptions {
        endpoint: position("--endpoint").and_then(|at| args.get(at + 1).cloned()),
        force: position("--force").is_some(),
        apply: position("--apply").is_some(),
    }
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
            let root = args.get(index + 3).cloned();
            std::process::exit(selftest(&model, &device, root.as_deref()));
        }
        if args.iter().any(|a| a == "--update-check") {
            app::run_update_probe(update_options(&args));
        }
        if let Some(index) = args.iter().position(|a| a == "--setup") {
            let target = args
                .get(index + 1)
                .cloned()
                .unwrap_or_else(|| "openvino".to_string());
            app::run_setup_probe(target);
        }
        app::run();
    }

    #[cfg(not(windows))]
    eprintln!(
        "meet-transcribe の GUI は Windows 専用です (WASAPI ループバック取り込みのため)。"
    );
}
