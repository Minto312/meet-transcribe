//! meet-transcribe GUI.
//!
//! A window around the same pipeline the `live` binary runs: pick a model and a
//! device, pick which audio endpoint to listen to (system loopback or a
//! microphone), start and stop, and keep every transcript in a sidebar.
//!
//! Windows-only, like the capture layer it drives.
//!
//! **The OpenVINO runtime must be on `PATH` before this starts.** The binary
//! links against `openvino_genai_c.dll`, so the DLLs have to resolve at process
//! load — too early for the app to fix it itself. Launch it via
//! `scripts/run_gui.ps1`, which runs `setupvars.bat` first.

// No console window in release; the in-app log panel is the user-facing log.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod commands;
#[cfg(windows)]
mod models;
#[cfg(windows)]
mod store;

fn main() {
    #[cfg(windows)]
    app::run();

    #[cfg(not(windows))]
    eprintln!(
        "meet-transcribe の GUI は Windows 専用です (WASAPI ループバック取り込みのため)。"
    );
}
