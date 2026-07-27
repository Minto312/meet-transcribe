//! WASAPI capture (Windows) -> 16 kHz mono f32.
//!
//! Two sources, one code path:
//!
//! - **Loopback** — take the default (or a chosen) *Render* endpoint and
//!   initialize its client with `Direction::Capture`. That is system-audio
//!   loopback: whatever the speakers are playing, with no Stereo Mix and no
//!   VB-CABLE virtual cable.
//! - **Microphone** — a *Capture* endpoint, initialized the ordinary way.
//!
//! Both end up calling `initialize_client(.., &Direction::Capture, ..)`; the
//! only difference is which endpoint the client came from. We request 16 kHz
//! float and let WASAPI's autoconvert resample for us (proper anti-aliased SRC),
//! then downmix channels to mono.
//!
//! Windows-only. Method names follow the wasapi crate's loopback/capture
//! examples; if the crate version differs, adjust against `cargo doc -p wasapi`.
//!
//! Nothing in `wasapi` is `Send`, so every COM object here is created and used
//! on one thread. That constraint shapes `engine.rs`: capture owns its thread.

use std::collections::VecDeque;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use wasapi::{
    initialize_mta, AudioCaptureClient, AudioClient, Device, DeviceEnumerator, Direction, Handle,
    SampleType, StreamMode, WaveFormat,
};

/// Which kind of endpoint to capture from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// A render (speaker/headphone) endpoint, captured in loopback mode.
    Loopback,
    /// A capture (microphone/line-in) endpoint.
    Microphone,
}

impl DeviceKind {
    fn direction(self) -> Direction {
        match self {
            Self::Loopback => Direction::Render,
            Self::Microphone => Direction::Capture,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Loopback => "ループバック",
            Self::Microphone => "マイク",
        }
    }
}

/// One selectable endpoint, as shown in the GUI's device list.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioDeviceInfo {
    /// WASAPI endpoint id. Stable across reboots, so it is what we persist.
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// True for the endpoint Windows currently treats as the default.
    pub is_default: bool,
}

/// What to open. `id: None` means "whatever Windows calls the default for this
/// kind right now" — the right behaviour when the user has never picked one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSelection {
    #[serde(default)]
    pub id: Option<String>,
    pub kind: DeviceKind,
}

impl Default for DeviceSelection {
    fn default() -> Self {
        Self {
            id: None,
            kind: DeviceKind::Loopback,
        }
    }
}

impl DeviceSelection {
    pub fn default_loopback() -> Self {
        Self::default()
    }
}

/// Enumerate every active render and capture endpoint.
///
/// COM is initialized on the *calling* thread, and everything returned is plain
/// `String`, so a caller can run this on a scratch thread and keep no COM
/// objects alive afterwards.
pub fn list_devices() -> Result<Vec<AudioDeviceInfo>> {
    // Ignore the error: COM may already be initialized on this thread.
    let _ = initialize_mta();
    let enumerator = DeviceEnumerator::new().context("DeviceEnumerator::new")?;

    let mut out = Vec::new();
    for kind in [DeviceKind::Loopback, DeviceKind::Microphone] {
        let direction = kind.direction();
        let default_id = enumerator
            .get_default_device(&direction)
            .ok()
            .and_then(|d| d.get_id().ok());

        // `get_device_collection` already filters to DEVICE_STATE_ACTIVE, so a
        // disabled or unplugged endpoint never reaches the list.
        let collection = enumerator
            .get_device_collection(&direction)
            .with_context(|| format!("enumerate {direction} endpoints"))?;
        for device in &collection {
            let Ok(device) = device else { continue };
            let Ok(id) = device.get_id() else { continue };
            let name = device
                .get_friendlyname()
                .unwrap_or_else(|_| "(名前不明のデバイス)".to_string());
            let is_default = default_id.as_deref() == Some(id.as_str());
            out.push(AudioDeviceInfo {
                id,
                name,
                kind,
                is_default,
            });
        }
    }
    Ok(out)
}

pub struct AudioCapture {
    client: AudioClient, // kept alive for the stream's lifetime
    capture: AudioCaptureClient,
    event: Handle,
    queue: VecDeque<u8>,
    channels: usize,
    block_align: usize,
    pub sample_rate: u32,
    pub device_name: String,
    pub device_id: String,
    pub kind: DeviceKind,
}

impl AudioCapture {
    /// Open the endpoint named by `selection`, or the current default for its
    /// kind when no id is given.
    pub fn open(selection: &DeviceSelection) -> Result<Self> {
        // Ignore the error: COM may already be initialized on this thread.
        let _ = initialize_mta();

        let enumerator = DeviceEnumerator::new().context("DeviceEnumerator::new")?;
        let direction = selection.kind.direction();

        let device = match &selection.id {
            Some(id) => {
                let device = enumerator.get_device(id).with_context(|| {
                    format!(
                        "オーディオデバイスが見つかりません (id={id})。取り外された可能性があります"
                    )
                })?;
                // A stale id can name an endpoint of the other direction, which
                // would silently capture the wrong thing.
                if device.get_direction() != direction {
                    return Err(anyhow!(
                        "選択されたデバイスの向きが一致しません: {} を期待しましたが {} でした",
                        direction,
                        device.get_direction()
                    ));
                }
                device
            }
            None => enumerator
                .get_default_device(&direction)
                .with_context(|| format!("既定の {direction} デバイスがありません"))?,
        };

        Self::from_device(device, selection.kind)
    }

    /// The original behaviour: the default speakers, in loopback.
    pub fn open_default_loopback() -> Result<Self> {
        Self::open(&DeviceSelection::default_loopback())
    }

    fn from_device(device: Device, kind: DeviceKind) -> Result<Self> {
        let device_name = device
            .get_friendlyname()
            .unwrap_or_else(|_| "unknown".to_string());
        let device_id = device.get_id().unwrap_or_default();

        let mut client = device.get_iaudioclient().context("get_iaudioclient")?;
        let channels = client.get_mixformat()?.get_nchannels() as usize;

        // Request 16 kHz / 32-bit float, same channel count; autoconvert resamples.
        let desired = WaveFormat::new(32, 32, &SampleType::Float, 16_000, channels, None);
        let (default_period, _min_period) = client.get_device_period()?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: default_period,
        };
        // Render endpoint + Direction::Capture == loopback of the system mix.
        // Capture endpoint + Direction::Capture == an ordinary microphone.
        client
            .initialize_client(&desired, &Direction::Capture, &mode)
            .with_context(|| format!("initialize_client ({device_name})"))?;

        let event = client.set_get_eventhandle()?;
        let capture = client.get_audiocaptureclient()?;
        let block_align = desired.get_blockalign() as usize;
        client.start_stream()?;

        Ok(Self {
            client,
            capture,
            event,
            queue: VecDeque::new(),
            channels,
            block_align,
            sample_rate: 16_000,
            device_name,
            device_id,
            kind,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Block until the next audio packet, then return it as 16 kHz mono f32.
    /// Returns an empty Vec on a silence timeout (WASAPI may not fire on silence),
    /// which is also what makes a capture loop interruptible within ~1 s.
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

impl Drop for AudioCapture {
    fn drop(&mut self) {
        // Releasing the client without stopping leaves the endpoint streaming
        // until COM tears it down; be explicit.
        let _ = self.client.stop_stream();
    }
}

/// Kept so existing call sites keep reading the same.
pub type LoopbackCapture = AudioCapture;
