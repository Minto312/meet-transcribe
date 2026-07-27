//! Audio helpers: channel downmix and resampling to 16 kHz.
//!
//! The live path resamples inside WASAPI (autoconvert), so it only needs the
//! downmix. `resample_to_16k` is used by the WAV-based benchmark. It is a plain
//! linear resampler (no anti-alias pre-filter) -- adequate for a prototype whose
//! job is timing, not word-error-rate. For production audio quality, swap in a
//! sinc resampler (e.g. the `rubato` crate).

/// Downmix interleaved f32 frames to mono by averaging channels.
pub fn downmix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Resample mono f32 from `src_sr` to 16 kHz (linear interpolation).
pub fn resample_to_16k(mono: &[f32], src_sr: u32) -> Vec<f32> {
    if src_sr == 16_000 || mono.len() < 2 {
        return mono.to_vec();
    }
    let ratio = 16_000_f64 / src_sr as f64;
    let out_len = ((mono.len() as f64) * ratio).round() as usize;
    let last = mono.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = mono[idx.min(last)];
        let b = mono[(idx + 1).min(last)];
        out.push(a + (b - a) * frac);
    }
    out
}
