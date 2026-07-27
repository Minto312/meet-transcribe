//! WASAPI loopback capture (Windows) -> 16 kHz mono f32.
//!
//! Captures whatever the DEFAULT SPEAKERS are playing -- no Stereo Mix, no
//! VB-CABLE virtual cable. The trick with the `wasapi` crate: take the default
//! *Render* device but initialize the client with `Direction::Capture`; that is
//! system-audio loopback. We request 16 kHz float and let WASAPI's autoconvert
//! resample for us (proper anti-aliased SRC), then downmix channels to mono.
//!
//! This module is the least-tested part of the port (it can only be built/run on
//! the ThinkPad). Method names follow the wasapi crate's loopback/capture
//! example; if the crate version differs, adjust against `cargo doc -p wasapi`.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use wasapi::{
    initialize_mta, AudioCaptureClient, AudioClient, DeviceEnumerator, Direction, Handle,
    SampleType, StreamMode, WaveFormat,
};

pub struct LoopbackCapture {
    _client: AudioClient, // kept alive for the stream's lifetime
    capture: AudioCaptureClient,
    event: Handle,
    queue: VecDeque<u8>,
    channels: usize,
    block_align: usize,
    pub sample_rate: u32,
    pub device_name: String,
}

impl LoopbackCapture {
    pub fn open() -> Result<Self> {
        // Ignore the error: COM may already be initialized on this thread.
        let _ = initialize_mta();

        let enumerator = DeviceEnumerator::new().context("DeviceEnumerator::new")?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .context("no default render (speaker) device")?;
        let device_name = device.get_friendlyname().unwrap_or_else(|_| "unknown".into());

        let mut client = device.get_iaudioclient().context("get_iaudioclient")?;
        let channels = client.get_mixformat()?.get_nchannels() as usize;

        // Request 16 kHz / 32-bit float, same channel count; autoconvert resamples.
        let desired = WaveFormat::new(32, 32, &SampleType::Float, 16_000, channels, None);
        let (default_period, _min_period) = client.get_device_period()?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: default_period,
        };
        // Render device + Direction::Capture == loopback of the system mix.
        client
            .initialize_client(&desired, &Direction::Capture, &mode)
            .context("initialize_client (loopback)")?;

        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        let block_align = desired.get_blockalign() as usize;
        client.start_stream()?;

        Ok(Self {
            _client: client,
            capture,
            event,
            queue: VecDeque::new(),
            channels,
            block_align,
            sample_rate: 16_000,
            device_name,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Block until the next audio packet, then return it as 16 kHz mono f32.
    /// Returns an empty Vec on a silence timeout (WASAPI may not fire on silence).
    pub fn read_chunk(&mut self) -> Result<Vec<f32>> {
        if self.event.wait_for_event(1000).is_err() {
            return Ok(Vec::new());
        }
        self.capture.read_from_device_to_deque(&mut self.queue)?;

        let frames = self.queue.len() / self.block_align;
        let mut mono = Vec::with_capacity(frames);
        for _ in 0..frames {
            let mut acc = 0.0_f32;
            for _ in 0..self.channels {
                let mut b = [0u8; 4];
                for byte in &mut b {
                    *byte = self.queue.pop_front().unwrap_or(0);
                }
                acc += f32::from_le_bytes(b);
            }
            mono.push(acc / self.channels as f32);
        }
        // Drop any partial-frame remainder so the byte stream stays aligned.
        let remainder = self.queue.len() % self.block_align;
        for _ in 0..remainder {
            self.queue.pop_front();
        }
        Ok(mono)
    }
}
