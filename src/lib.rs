//! meet-transcribe: desktop-audio realtime transcription on Intel Core Ultra.
//!
//! The whole point is to measure whether the 155H's NPU can keep up with live
//! audio (RTF < 1) while leaving the iGPU free for the desktop + video. `asr`
//! wraps OpenVINO GenAI with the device as a single argument you can flip.

pub mod asr;
pub mod audio;
pub mod vad;

#[cfg(windows)]
pub mod capture;
