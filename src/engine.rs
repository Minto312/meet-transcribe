//! Headless orchestration of the live pipeline, split across threads.
//!
//! The CLI (`live`) can afford one blocking loop. A GUI cannot: on the NPU,
//! `WhisperPipeline::new` takes ~60 s for `small` and ~170 s for `medium`, and
//! the result is *not* cached between processes (openvino.genai #1992). So the
//! engine is built around that cost:
//!
//! ```text
//!   control thread ──select──┬── EngineCommand  (from the UI)
//!                            └── Notice         (from the workers)
//!         │ spawns
//!         ├── capture thread   WASAPI -> Silero VAD -> utterances
//!         │      (per run; every COM object in `wasapi` is !Send, so the
//!         │       device must be opened on the thread that reads it)
//!         └── ASR thread       long-lived; owns the compiled pipeline and
//!                              KEEPS IT across stop/start, so a second run of
//!                              the same model+device starts instantly
//! ```
//!
//! Utterances flow capture -> ASR over a *bounded* channel. If the device
//! cannot keep up, we drop utterances and say so, rather than growing a queue
//! until the process dies. Because that one channel carries both `Load` and
//! `Work` in FIFO order, queued audio is automatically transcribed only after
//! the model finishes compiling — no separate gate needed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender, TrySendError};
use serde::{Deserialize, Serialize};

use crate::asr::WhisperAsr;
use crate::capture::{AudioCapture, DeviceKind, DeviceSelection};
use crate::vad::SpeechSegmenter;

pub const DEFAULT_MAX_SEGMENT_S: f32 = 15.0;
pub const DEFAULT_VAD_THRESHOLD: f32 = 0.5;
pub const DEFAULT_MIN_SILENCE_MS: u32 = 500;

/// Utterances allowed to wait for the ASR device. 128 utterances of the ~15 s
/// maximum is a ~120 MB worst case, which bounds the damage if the chosen
/// device turns out to be slower than real time.
const ASR_QUEUE: usize = 128;

/// How often the capture thread reports signal level to the UI.
const LEVEL_INTERVAL: Duration = Duration::from_millis(120);

// ---------------------------------------------------------------- public types

/// A model + the device and language it is compiled for. Equality is what
/// decides whether a reload is needed, so every field that affects the
/// compiled pipeline belongs here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub dir: PathBuf,
    /// `NPU` | `GPU` | `CPU`
    pub device: String,
    /// Whisper language code, e.g. `ja`.
    pub language: String,
}

impl ModelSpec {
    /// The directory's own name, which is how models are labelled in the UI.
    pub fn name(&self) -> String {
        self.dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.dir.display().to_string())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunConfig {
    pub model: ModelSpec,
    pub audio: DeviceSelection,
    #[serde(default = "default_max_segment")]
    pub max_segment_s: f32,
    #[serde(default = "default_threshold")]
    pub vad_threshold: f32,
    #[serde(default = "default_min_silence")]
    pub min_silence_ms: u32,
}

fn default_max_segment() -> f32 {
    DEFAULT_MAX_SEGMENT_S
}
fn default_threshold() -> f32 {
    DEFAULT_VAD_THRESHOLD
}
fn default_min_silence() -> u32 {
    DEFAULT_MIN_SILENCE_MS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Idle,
    /// Compiling the model onto the device. Minutes on the NPU.
    LoadingModel,
    /// Capturing and transcribing.
    Running,
    /// Capture has stopped, but utterances already captured are still queued.
    Finalizing,
}

impl Default for Phase {
    fn default() -> Self {
        Self::Idle
    }
}

/// Everything the UI needs to render the control bar. Pushed on every change.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub phase: Phase,
    /// Name of the model currently loaded *or being loaded*.
    pub model: Option<String>,
    pub device: Option<String>,
    pub language: Option<String>,
    pub audio_device: Option<String>,
    pub audio_kind: Option<DeviceKind>,
    /// True once a pipeline is compiled and resident. Survives stop/start.
    pub model_loaded: bool,
    /// Seconds the last successful compile took.
    pub load_seconds: Option<f64>,
    /// Utterances captured but not yet transcribed.
    pub queued: u64,
    /// Utterances thrown away because the queue was full.
    pub dropped: u64,
    /// Utterances transcribed in the current run.
    pub segments: u64,
    /// Last fatal-ish problem, cleared on the next successful start.
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecord {
    pub level: LogLevel,
    pub message: String,
}

impl LogRecord {
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Info,
            message: message.into(),
        }
    }
    pub fn warn(message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Warn,
            message: message.into(),
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            level: LogLevel::Error,
            message: message.into(),
        }
    }
}

/// One transcribed utterance.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentEvent {
    /// Sequence number within the current run.
    pub index: u64,
    /// Where the utterance started, in seconds from the start of capture.
    pub offset_s: f64,
    pub audio_s: f64,
    pub infer_s: f64,
    /// Real-Time Factor: compute time / audio duration. Below 1.0 keeps up.
    pub rtf: f64,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelEvent {
    pub rms: f32,
    pub peak: f32,
    /// True while the VAD is inside an utterance.
    pub speech: bool,
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Status(EngineStatus),
    Log(LogRecord),
    Segment(SegmentEvent),
    Level(LevelEvent),
}

pub enum EngineCommand {
    /// Compile a model onto a device without starting capture.
    ///
    /// Worth its own command: NPU compilation is minutes and is not cached
    /// between processes, so doing it before the meeting starts is the
    /// difference between "press start" and "press start and wait 3 minutes".
    Preload(ModelSpec),
    Start(Box<RunConfig>),
    Stop,
    Shutdown,
}

/// Handle to the engine. Cheap to clone the sender side via [`Engine::send`].
pub struct Engine {
    cmd_tx: Sender<EngineCommand>,
}

impl Engine {
    /// Start the control thread. The returned receiver is the *only* consumer
    /// of engine events; hand it to one bridge/pump.
    pub fn spawn() -> (Self, Receiver<EngineEvent>) {
        let (cmd_tx, cmd_rx) = unbounded::<EngineCommand>();
        let (evt_tx, evt_rx) = unbounded::<EngineEvent>();
        thread::Builder::new()
            .name("mt-engine".to_string())
            .spawn(move || control_loop(cmd_rx, evt_tx))
            .expect("spawn engine control thread");
        (Self { cmd_tx }, evt_rx)
    }

    pub fn send(&self, cmd: EngineCommand) {
        // A dead engine thread means the app is shutting down; nothing to do.
        let _ = self.cmd_tx.send(cmd);
    }
}

// ------------------------------------------------------------- internal wiring

enum AsrJob {
    Load(ModelSpec),
    Work {
        index: u64,
        offset_s: f64,
        samples: Vec<f32>,
    },
    Shutdown,
}

enum Notice {
    Loaded {
        spec: ModelSpec,
        seconds: f64,
    },
    LoadFailed {
        error: String,
    },
    /// One `Work` job left the ASR thread, successfully or not.
    Done,
    CaptureOpened {
        name: String,
        kind: DeviceKind,
        channels: usize,
    },
    CaptureFailed {
        error: String,
    },
    CaptureStopped {
        submitted: u64,
    },
}

struct CaptureRun {
    running: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct Control {
    evt_tx: Sender<EngineEvent>,
    notice_tx: Sender<Notice>,
    asr_tx: Sender<AsrJob>,
    status: EngineStatus,
    /// What the ASR thread is holding right now, as far as we asked it to.
    loaded: Option<ModelSpec>,
    /// A `Load` we have sent but not yet heard back on. Guards against asking
    /// the NPU to compile the same model twice because the user pressed
    /// "start" while a preload of it was still running.
    pending_load: Option<ModelSpec>,
    capture: Option<CaptureRun>,
    /// Kept after a stop so the next start can join it before reopening the
    /// same endpoint.
    reaping: Option<JoinHandle<()>>,
    submitted: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    done: u64,
    /// True once the capture thread has promised to submit nothing more.
    capture_done: bool,
}

fn control_loop(cmd_rx: Receiver<EngineCommand>, evt_tx: Sender<EngineEvent>) {
    let (notice_tx, notice_rx) = unbounded::<Notice>();
    let (asr_tx, asr_rx) = bounded::<AsrJob>(ASR_QUEUE);

    {
        let notice_tx = notice_tx.clone();
        let evt_tx = evt_tx.clone();
        thread::Builder::new()
            .name("mt-asr".to_string())
            .spawn(move || asr_worker(asr_rx, notice_tx, evt_tx))
            .expect("spawn ASR thread");
    }

    let mut ctl = Control {
        evt_tx,
        notice_tx,
        asr_tx,
        status: EngineStatus::default(),
        loaded: None,
        pending_load: None,
        capture: None,
        reaping: None,
        submitted: Arc::new(AtomicU64::new(0)),
        dropped: Arc::new(AtomicU64::new(0)),
        done: 0,
        capture_done: true,
    };
    ctl.push_status();

    loop {
        select! {
            recv(cmd_rx) -> cmd => match cmd {
                Ok(EngineCommand::Shutdown) | Err(_) => {
                    ctl.stop_capture();
                    let _ = ctl.asr_tx.try_send(AsrJob::Shutdown);
                    break;
                }
                Ok(cmd) => ctl.handle_command(cmd),
            },
            recv(notice_rx) -> notice => match notice {
                Ok(notice) => ctl.handle_notice(notice),
                // `ctl` owns a sender, so this only happens if the channel is
                // poisoned by a panic elsewhere.
                Err(_) => break,
            },
        }
    }
    // The ASR thread is deliberately *not* joined: it may be minutes into an
    // uncancellable NPU compile, and the process is going away anyway.
}

impl Control {
    fn emit(&self, event: EngineEvent) {
        let _ = self.evt_tx.send(event);
    }

    fn log(&self, record: LogRecord) {
        self.emit(EngineEvent::Log(record));
    }

    /// Utterances captured but not yet handled by the ASR thread.
    fn outstanding(&self) -> u64 {
        self.submitted
            .load(Ordering::Relaxed)
            .saturating_sub(self.done)
    }

    fn push_status(&mut self) {
        self.status.queued = self.outstanding();
        self.status.dropped = self.dropped.load(Ordering::Relaxed);
        self.status.segments = self.done;
        self.emit(EngineEvent::Status(self.status.clone()));
    }

    fn handle_command(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::Preload(spec) => self.preload(spec),
            EngineCommand::Start(config) => self.start(*config),
            EngineCommand::Stop => self.stop(),
            // Handled by the caller so it can break the loop.
            EngineCommand::Shutdown => {}
        }
    }

    fn preload(&mut self, spec: ModelSpec) {
        if self.capture.is_some() {
            self.log(LogRecord::warn(
                "文字起こし中はモデルを変更できません。いったん停止してください。",
            ));
            return;
        }
        if self.loaded.as_ref() == Some(&spec) {
            self.log(LogRecord::info(format!(
                "{} は {} に読み込み済みです",
                spec.name(),
                spec.device
            )));
            return;
        }
        self.request_load(spec);
        self.push_status();
    }

    /// Ask the ASR thread for a pipeline, unless it already has it or is
    /// already building it.
    fn request_load(&mut self, spec: ModelSpec) {
        self.status.model = Some(spec.name());
        self.status.device = Some(spec.device.clone());
        self.status.language = Some(spec.language.clone());

        if self.loaded.as_ref() == Some(&spec) {
            self.status.model_loaded = true;
            return;
        }
        if self.pending_load.as_ref() == Some(&spec) {
            // Already compiling this exact spec; asking again would make the
            // NPU do the whole ~170 s a second time.
            self.status.phase = Phase::LoadingModel;
            return;
        }

        self.status.model_loaded = false;
        self.status.load_seconds = None;
        self.status.phase = Phase::LoadingModel;
        self.loaded = None;
        self.pending_load = Some(spec.clone());
        self.log(LogRecord::info(format!(
            "{} を {} に読み込んでいます (NPU では数分かかります)",
            spec.name(),
            spec.device
        )));
        if self.asr_tx.try_send(AsrJob::Load(spec)).is_err() {
            self.pending_load = None;
            self.status.phase = Phase::Idle;
            self.status.error = Some("ASR キューが詰まっており読み込みを開始できません".into());
            self.log(LogRecord::error(
                "ASR キューが詰まっており読み込みを開始できません",
            ));
        }
    }

    fn start(&mut self, config: RunConfig) {
        if self.capture.is_some() {
            self.log(LogRecord::warn("すでに文字起こし中です"));
            return;
        }
        if self.status.phase == Phase::Finalizing {
            self.log(LogRecord::warn(
                "前回分の文字起こしがまだ残っています。完了までお待ちください。",
            ));
            return;
        }

        // Release the previous endpoint before reopening one.
        if let Some(handle) = self.reaping.take() {
            let _ = handle.join();
        }

        self.status.error = None;
        self.submitted = Arc::new(AtomicU64::new(0));
        self.dropped = Arc::new(AtomicU64::new(0));
        self.done = 0;
        self.capture_done = false;

        self.status.language = Some(config.model.language.clone());
        self.request_load(config.model.clone());
        if self.status.phase != Phase::LoadingModel {
            self.status.phase = Phase::Running;
        }

        self.status.audio_kind = Some(config.audio.kind);
        self.status.audio_device = None;

        let running = Arc::new(AtomicBool::new(true));
        let params = CaptureParams {
            audio: config.audio.clone(),
            vad_threshold: config.vad_threshold,
            min_silence_ms: config.min_silence_ms,
            max_segment_s: config.max_segment_s,
        };
        let handle = thread::Builder::new()
            .name("mt-capture".to_string())
            .spawn({
                let running = Arc::clone(&running);
                let submitted = Arc::clone(&self.submitted);
                let dropped = Arc::clone(&self.dropped);
                let asr_tx = self.asr_tx.clone();
                let notice_tx = self.notice_tx.clone();
                let evt_tx = self.evt_tx.clone();
                move || capture_worker(params, running, submitted, dropped, asr_tx, notice_tx, evt_tx)
            })
            .expect("spawn capture thread");

        self.capture = Some(CaptureRun { running, handle });
        self.push_status();
    }

    fn stop_capture(&mut self) {
        if let Some(run) = self.capture.take() {
            run.running.store(false, Ordering::Relaxed);
            // Not joined here: `read_chunk` can be up to ~1 s from returning and
            // the control thread must stay responsive. The thread reports
            // `CaptureStopped` itself; the handle is joined before the next start.
            self.reaping = Some(run.handle);
        }
    }

    fn stop(&mut self) {
        if self.capture.is_none() {
            return;
        }
        self.stop_capture();
        // Deliberately does *not* clobber `LoadingModel`: the compile keeps
        // running, and the audio captured so far is transcribed once it lands.
        if self.status.phase == Phase::Running {
            self.status.phase = Phase::Finalizing;
        }
        self.push_status();
    }

    /// Move out of `Finalizing` once every captured utterance has been handled.
    /// Only `capture_done` proves nothing more will be submitted — the capture
    /// thread can still push one last utterance after `running` goes false.
    fn settle(&mut self) {
        if self.capture.is_some() || !self.capture_done || self.outstanding() > 0 {
            return;
        }
        if self.status.phase == Phase::Finalizing {
            self.status.phase = Phase::Idle;
        }
        if self.status.phase == Phase::Idle {
            self.status.audio_device = None;
            self.status.audio_kind = None;
        }
    }

    fn handle_notice(&mut self, notice: Notice) {
        match notice {
            Notice::Loaded { spec, seconds } => {
                self.log(LogRecord::info(format!(
                    "{} を {} に読み込みました ({seconds:.1} 秒)",
                    spec.name(),
                    spec.device
                )));
                self.pending_load = None;
                self.loaded = Some(spec);
                self.status.model_loaded = true;
                self.status.load_seconds = Some(seconds);
                if self.status.phase == Phase::LoadingModel {
                    self.status.phase = if self.capture.is_some() {
                        Phase::Running
                    } else if self.outstanding() > 0 {
                        Phase::Finalizing
                    } else {
                        Phase::Idle
                    };
                }
                self.settle();
                self.push_status();
            }
            Notice::LoadFailed { error } => {
                self.log(LogRecord::error(format!("モデルの読み込みに失敗: {error}")));
                self.pending_load = None;
                self.loaded = None;
                self.status.model_loaded = false;
                self.status.error = Some(error);
                // Without a pipeline there is nothing to transcribe into, and
                // the queued audio will be discarded as it drains.
                self.stop_capture();
                self.status.phase = Phase::Idle;
                self.push_status();
            }
            Notice::Done => {
                self.done += 1;
                self.settle();
                self.push_status();
            }
            Notice::CaptureOpened {
                name,
                kind,
                channels,
            } => {
                self.log(LogRecord::info(format!(
                    "{} を取り込み中: {name} ({channels} ch → 16 kHz mono)",
                    kind.label()
                )));
                self.status.audio_device = Some(name);
                self.status.audio_kind = Some(kind);
                self.push_status();
            }
            Notice::CaptureFailed { error } => {
                self.log(LogRecord::error(format!(
                    "オーディオデバイスを開けませんでした: {error}"
                )));
                self.status.error = Some(error);
                if let Some(run) = self.capture.take() {
                    self.reaping = Some(run.handle);
                }
                self.capture_done = true;
                if self.status.phase == Phase::Running {
                    self.status.phase = Phase::Idle;
                }
                self.settle();
                self.push_status();
            }
            Notice::CaptureStopped { submitted } => {
                debug_assert!(submitted >= self.done);
                if let Some(run) = self.capture.take() {
                    self.reaping = Some(run.handle);
                }
                self.capture_done = true;
                if self.status.phase == Phase::Running {
                    self.status.phase = Phase::Finalizing;
                }
                self.settle();
                self.push_status();
            }
        }
    }
}

// ------------------------------------------------------------------ ASR thread

fn asr_worker(rx: Receiver<AsrJob>, notice: Sender<Notice>, evt: Sender<EngineEvent>) {
    // Held across runs on purpose: recompiling `medium` on the NPU costs ~170 s,
    // so a stop/start of the same model must not pay it twice.
    let mut current: Option<(ModelSpec, WhisperAsr)> = None;

    while let Ok(job) = rx.recv() {
        match job {
            AsrJob::Shutdown => break,

            AsrJob::Load(spec) => {
                if current.as_ref().map(|(s, _)| s) == Some(&spec) {
                    let _ = notice.send(Notice::Loaded {
                        spec,
                        seconds: 0.0,
                    });
                    continue;
                }
                // Drop the old pipeline first: the NPU will not hand out memory
                // for a second compiled model while one is still resident.
                current = None;

                let Some(dir) = spec.dir.to_str().map(str::to_owned) else {
                    let _ = notice.send(Notice::LoadFailed {
                        error: format!("モデルのパスが UTF-8 ではありません: {}", spec.dir.display()),
                    });
                    continue;
                };

                let t0 = Instant::now();
                match WhisperAsr::new(&dir, &spec.device, &spec.language) {
                    Ok(asr) => {
                        let seconds = t0.elapsed().as_secs_f64();
                        current = Some((spec.clone(), asr));
                        let _ = notice.send(Notice::Loaded { spec, seconds });
                    }
                    Err(e) => {
                        let _ = notice.send(Notice::LoadFailed {
                            error: format!("{e:#}"),
                        });
                    }
                }
            }

            AsrJob::Work {
                index,
                offset_s,
                samples,
            } => {
                match current.as_mut() {
                    Some((_, asr)) => match asr.transcribe(&samples) {
                        Ok(result) => {
                            if !result.text.is_empty() {
                                let _ = evt.send(EngineEvent::Segment(SegmentEvent {
                                    index,
                                    offset_s,
                                    audio_s: result.audio_seconds,
                                    infer_s: result.infer_seconds,
                                    rtf: result.rtf(),
                                    text: result.text,
                                }));
                            }
                        }
                        Err(e) => {
                            let _ = evt.send(EngineEvent::Log(LogRecord::error(format!(
                                "文字起こしに失敗しました (#{index}): {e:#}"
                            ))));
                        }
                    },
                    // The load failed; this audio can never be transcribed. Still
                    // report Done so the controller's finalize counter completes.
                    None => {}
                }
                let _ = notice.send(Notice::Done);
            }
        }
    }
}

// -------------------------------------------------------------- capture thread

struct CaptureParams {
    audio: DeviceSelection,
    vad_threshold: f32,
    min_silence_ms: u32,
    max_segment_s: f32,
}

fn capture_worker(
    params: CaptureParams,
    running: Arc<AtomicBool>,
    submitted: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    asr_tx: Sender<AsrJob>,
    notice: Sender<Notice>,
    evt: Sender<EngineEvent>,
) {
    let mut capture = match AudioCapture::open(&params.audio) {
        Ok(capture) => capture,
        Err(e) => {
            let _ = notice.send(Notice::CaptureFailed {
                error: format!("{e:#}"),
            });
            return;
        }
    };
    let mut segmenter = match SpeechSegmenter::new(
        params.vad_threshold,
        params.min_silence_ms,
        params.max_segment_s,
    ) {
        Ok(segmenter) => segmenter,
        Err(e) => {
            let _ = notice.send(Notice::CaptureFailed {
                error: format!("VAD の初期化に失敗しました: {e:#}"),
            });
            return;
        }
    };

    let _ = notice.send(Notice::CaptureOpened {
        name: capture.device_name.clone(),
        kind: capture.kind,
        channels: capture.channels(),
    });

    let mut last_level = Instant::now();
    let mut sum_sq = 0.0_f64;
    let mut samples_seen = 0_usize;
    let mut peak = 0.0_f32;

    'capture: while running.load(Ordering::Relaxed) {
        let chunk = match capture.read_chunk() {
            Ok(chunk) => chunk,
            Err(e) => {
                let _ = evt.send(EngineEvent::Log(LogRecord::error(format!(
                    "音声の取り込みが中断されました: {e:#}"
                ))));
                break;
            }
        };

        if !chunk.is_empty() {
            for &v in &chunk {
                let magnitude = v.abs();
                if magnitude > peak {
                    peak = magnitude;
                }
                sum_sq += (v as f64) * (v as f64);
            }
            samples_seen += chunk.len();

            for utterance in segmenter.feed(&chunk) {
                // Reserve the index only if the send succeeds, so indices stay
                // dense and the finalize counter stays exact.
                let index = submitted.load(Ordering::Relaxed);
                let job = AsrJob::Work {
                    index,
                    offset_s: utterance.start_seconds(),
                    samples: utterance.samples,
                };
                match asr_tx.try_send(job) {
                    Ok(()) => submitted.store(index + 1, Ordering::Relaxed),
                    Err(TrySendError::Full(_)) => {
                        let total = dropped.fetch_add(1, Ordering::Relaxed) + 1;
                        let _ = evt.send(EngineEvent::Log(LogRecord::warn(format!(
                            "ASR が追いつかないため 1 発話を破棄しました (累計 {total} 件)"
                        ))));
                    }
                    Err(TrySendError::Disconnected(_)) => break 'capture,
                }
            }
        }

        if last_level.elapsed() >= LEVEL_INTERVAL {
            let rms = if samples_seen > 0 {
                (sum_sq / samples_seen as f64).sqrt() as f32
            } else {
                0.0
            };
            let _ = evt.send(EngineEvent::Level(LevelEvent {
                rms,
                peak,
                speech: segmenter.in_speech(),
            }));
            sum_sq = 0.0;
            samples_seen = 0;
            peak = 0.0;
            last_level = Instant::now();
        }
    }

    let _ = notice.send(Notice::CaptureStopped {
        submitted: submitted.load(Ordering::Relaxed),
    });
}
