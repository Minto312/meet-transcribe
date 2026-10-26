//! meet-transcribe: desktop-audio realtime transcription on Intel Core Ultra.
//!
//! The whole point is to measure whether the 155H's NPU can keep up with live
//! audio (RTF < 1) while leaving the iGPU free for the desktop + video. `asr`
//! wraps OpenVINO GenAI with the device as a single argument you can flip.
//!
//! `engine` layers the threading a GUI needs on top of the same pieces: it is
//! what `app/` (the Tauri front end) drives, and it exists mainly because NPU
//! model compilation takes minutes and must not block a window.

pub mod asr;
pub mod audio;
pub mod batch;
pub mod repeat;
pub mod vad;

#[cfg(windows)]
pub mod capture;

#[cfg(windows)]
pub mod engine;
