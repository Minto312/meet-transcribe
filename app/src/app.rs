//! Tauri wiring: shared state, the engine→webview bridge, and app startup.

use std::path::PathBuf;
use std::sync::Mutex;

use crossbeam_channel::Receiver;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use meet_transcribe::engine::{
    Engine, EngineCommand, EngineEvent, EngineStatus, LogRecord, ModelSpec, Phase,
};

use crate::autostart;
use crate::models;
use crate::openvino::OpenvinoStatus;
use crate::setup::{self, Paths, SetupState};
use crate::store::{SegmentRecord, SessionStore, Settings};
use crate::update;

pub struct AppState {
    pub engine: Engine,
    pub store: SessionStore,
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    /// Whether the OpenVINO runtime was found. Decided at startup, before any
    /// thread exists, because resolving it mutates the process PATH — and
    /// decided again if the setup panel downloads it (see `setup.rs`).
    pub openvino: Mutex<OpenvinoStatus>,
    /// Mirror of the last status pushed by the engine, so a freshly opened or
    /// reloaded window can ask for it instead of waiting for the next change.
    pub status: Mutex<EngineStatus>,
    /// Downloads of the OpenVINO runtime and the Whisper models.
    pub setup: SetupState,
    /// Result of the last update check, so installing does not re-ask the feed.
    pub pending_update: update::Pending,
}

impl AppState {
    pub fn persist_settings(&self) -> anyhow::Result<()> {
        let settings = self.settings.lock().unwrap().clone();
        settings.save(&self.settings_path)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentPayload {
    session_id: String,
    segment: SegmentRecord,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionFinished {
    id: String,
    /// True when the run produced nothing and the file was discarded, so the
    /// sidebar does not fill up with empty entries after a failed start.
    deleted: bool,
}

/// Pump engine events into the webview, persisting transcripts on the way past.
///
/// Writing to disk *before* emitting is deliberate: what you see in the window
/// is already saved.
fn bridge(app: AppHandle, events: Receiver<EngineEvent>) {
    while let Ok(event) = events.recv() {
        let state = app.state::<AppState>();
        match event {
            EngineEvent::Status(status) => {
                let idle = status.phase == Phase::Idle;
                *state.status.lock().unwrap() = status.clone();
                let _ = app.emit("mt://status", status);

                // The run is only over once the ASR queue has drained, which is
                // exactly when the engine returns to Idle.
                if idle && state.store.active_id().is_some() {
                    match state.store.finish() {
                        Ok(Some((id, segments))) => {
                            let deleted = segments == 0 && state.store.delete(&id).is_ok();
                            let _ = app.emit("mt://session-finished", SessionFinished { id, deleted });
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let _ = app.emit(
                                "mt://log",
                                LogRecord::error(format!("セッションの保存に失敗しました: {e}")),
                            );
                        }
                    }
                }
            }

            EngineEvent::Log(record) => {
                let _ = app.emit("mt://log", record);
            }

            EngineEvent::Level(level) => {
                let _ = app.emit("mt://level", level);
            }

            EngineEvent::Segment(segment) => match state.store.append(&segment) {
                Ok(Some((session_id, record))) => {
                    let _ = app.emit(
                        "mt://segment",
                        SegmentPayload {
                            session_id,
                            segment: record,
                        },
                    );
                }
                // Nothing is being recorded (a late utterance after finish).
                // Losing it silently would be worse than saying so.
                Ok(None) => {
                    let _ = app.emit(
                        "mt://log",
                        LogRecord::warn(format!(
                            "記録中のセッションがないため保存されませんでした: {}",
                            segment.text
                        )),
                    );
                }
                Err(e) => {
                    let _ = app.emit(
                        "mt://log",
                        LogRecord::error(format!("文字起こしの保存に失敗しました: {e}")),
                    );
                }
            },
        }
    }
}

/// What every entry point shares: state, plugins, commands.
///
/// `on_ready` runs once the app is built. The window does the normal thing; the
/// `--update-check` probe uses it to drive the updater from a console and exit.
fn build(on_ready: impl FnOnce(&AppHandle) + Send + 'static) -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // Configured entirely from `tauri.conf.json` (`plugins > updater`); the
        // app talks to it from Rust, so no ACL permission is needed.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            // Transcripts and settings stay in the roaming profile, where they
            // have always been. The downloads are hundreds of megabytes of
            // machine-specific binaries and belong in the local one.
            let data_dir = app.path().app_data_dir()?;
            let local_dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let paths = Paths::new(&local_dir);
            std::fs::create_dir_all(&paths.models)?;

            let settings_path = data_dir.join("settings.json");
            let mut settings = Settings::load(&settings_path);
            if settings.models_dir.is_none() {
                // In a checkout this finds the repository's own models/; an
                // installed copy has none, and falls back to the folder the
                // setup panel downloads into.
                settings.models_dir =
                    Some(models::guess_models_dir().unwrap_or_else(|| paths.models.clone()));
            }
            let _ = settings.save(&settings_path);

            let store = SessionStore::new(data_dir.join("sessions"))?;

            // Before any thread exists: this prepends to the process PATH so the
            // delay-loaded OpenVINO import can resolve when the ASR thread first
            // needs it.
            let openvino =
                crate::openvino::prepare(settings.openvino_dir.as_deref(), Some(&paths.openvino));

            let (engine, events) = Engine::spawn();

            app.manage(AppState {
                engine,
                store,
                settings: Mutex::new(settings),
                settings_path,
                openvino: Mutex::new(openvino),
                status: Mutex::new(EngineStatus::default()),
                setup: SetupState::new(paths),
                pending_update: Mutex::new(None),
            });

            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("mt-bridge".to_string())
                .spawn(move || bridge(handle, events))?;

            on_ready(app.handle());

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Close the transcript cleanly. Anything still queued on the
                // NPU is lost -- an uncancellable inference cannot be waited
                // out at exit -- but everything already transcribed is on disk.
                let state = window.state::<AppState>();
                state.engine.send(EngineCommand::Shutdown);
                let _ = state.store.finish();
            }
        })
        .invoke_handler(tauri::generate_handler![
            crate::commands::get_settings,
            crate::commands::set_settings,
            crate::commands::openvino_status,
            crate::commands::set_openvino_dir,
            crate::commands::list_audio_devices,
            crate::commands::list_models,
            crate::commands::set_models_dir,
            crate::commands::get_status,
            crate::commands::preload_model,
            crate::commands::start_transcription,
            crate::commands::stop_transcription,
            crate::commands::active_session,
            crate::commands::list_sessions,
            crate::commands::get_session,
            crate::commands::rename_session,
            crate::commands::delete_session,
            crate::commands::session_markdown,
            crate::commands::save_session_as,
            crate::commands::sessions_dir,
            crate::commands::setup_status,
            crate::commands::download_openvino,
            crate::commands::download_model,
            crate::commands::cancel_setup,
            crate::commands::autostart_enabled,
            crate::commands::set_autostart,
            crate::commands::app_version,
            crate::commands::check_update,
            crate::commands::install_update,
        ])
        .run(tauri::generate_context!())
}

pub fn run() {
    let autostarted = std::env::args().any(|a| a == autostart::FLAG);
    build(move |app| {
        if autostarted {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.minimize();
            }
        }
        update::spawn_startup_check(app.clone());
        let _ = preload_at_launch(app);
    })
    .expect("meet-transcribe GUI の起動に失敗しました");
}

/// Compile the model the app was last used with, as soon as it starts.
///
/// NPU compilation is minutes and is not cached between processes, so this is
/// what makes 開始 instant in a meeting — provided the app started well before
/// it, which is what `autostart` is for. Nobody is watching when it runs, so it
/// stays out of anything the window would have refused or warned about.
/// Returns what it asked the engine to load, or why it did not.
fn preload_at_launch(app: &AppHandle) -> Result<ModelSpec, &'static str> {
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap().clone();
    if !settings.preload_on_launch {
        return Err("起動時の先読みが無効になっています");
    }
    if !matches!(*state.openvino.lock().unwrap(), OpenvinoStatus::Ready { .. }) {
        return Err("OpenVINO ランタイムが見つかりません");
    }
    let (Some(models_dir), Some(model)) = (settings.models_dir, settings.model) else {
        return Err("モデルがまだ選ばれていません");
    };
    let dir = models_dir.join(model);
    if !models::is_preloadable(&dir, &settings.device) {
        return Err("選ばれているモデルはこのデバイスで読み込めません");
    }
    let spec = ModelSpec {
        dir,
        device: settings.device,
        language: settings.language,
    };
    state.engine.send(EngineCommand::Preload(spec.clone()));
    Ok(spec)
}

/// `--startup-check [model-name device]`: what happens at sign-in, from a
/// console.
///
/// Signing in cannot be staged over SSH, so this checks the two halves of it:
/// that the `Run` entry can be written, read back and removed again (leaving it
/// as it was found), and that the preload at launch really reaches the engine
/// and ends with the model resident. The optional arguments replace the saved
/// model and device for this run only, so a small model can stand in for the
/// minutes `medium` takes on the NPU.
pub fn run_startup_probe(model: Option<(String, String)>) -> ! {
    let result = build(move |app| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
        let app = app.clone();
        std::thread::Builder::new()
            .name("mt-startup-probe".to_string())
            .spawn(move || std::process::exit(startup_probe(&app, model)))
            .expect("プローブスレッドを起動できません");
    });

    if let Err(error) = result {
        eprintln!("--startup-check を実行できません: {error}");
    }
    std::process::exit(1);
}

fn startup_probe(app: &AppHandle, model: Option<(String, String)>) -> i32 {
    let mut failed = false;

    let saved = autostart::save();
    let before = autostart::is_enabled();
    let on = autostart::set_enabled(true).map(|()| autostart::is_enabled());
    let off = autostart::set_enabled(false).map(|()| autostart::is_enabled());
    let restored = autostart::restore(&saved).map(|()| autostart::is_enabled());
    println!(
        "autostart: before={before} register->{on:?} unregister->{off:?} restored->{restored:?}"
    );
    if !matches!((&on, &off), (Ok(true), Ok(false)))
        || !matches!(&restored, Ok(after) if *after == before)
    {
        failed = true;
    }

    let state = app.state::<AppState>();
    if let Some((name, device)) = model {
        let mut settings = state.settings.lock().unwrap();
        settings.model = Some(name);
        settings.device = device;
        settings.preload_on_launch = true;
    }
    let started = std::time::Instant::now();
    match preload_at_launch(app) {
        Ok(spec) => println!("preload: asked for {} on {}", spec.name(), spec.device),
        Err(reason) => {
            println!("preload: not started: {reason}");
            println!("VERDICT: FAILED");
            return 1;
        }
    }
    // The bridge mirrors every status the engine pushes into `state.status`.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let status = state.status.lock().unwrap().clone();
        if status.model_loaded {
            println!(
                "preload: resident after {:.1} s (compile {:.1} s), phase {:?}",
                started.elapsed().as_secs_f64(),
                status.load_seconds.unwrap_or(f64::NAN),
                status.phase
            );
            break;
        }
        if let Some(error) = status.error {
            println!("preload: FAILED {error}");
            failed = true;
            break;
        }
        if started.elapsed() > std::time::Duration::from_secs(600) {
            println!("preload: still not resident after 600 s");
            failed = true;
            break;
        }
    }

    println!("VERDICT: {}", if failed { "FAILED" } else { "sign-in path works." });
    i32::from(failed)
}

/// `--setup <openvino|OpenVINO/whisper-…>`: run one setup download from a
/// console and exit.
///
/// Drives the same commands the window does — `setup::start_openvino` /
/// `start_model` — and follows the run by subscribing to the same
/// `mt://setup-progress` events the front end gets, so what this proves is the
/// production path and not a parallel one. That matters for the extraction in
/// particular: picking 24 entries out of 1448 by prefix is exactly the kind of
/// thing that keeps working right up until the archive is reorganised.
pub fn run_setup_probe(target: String) -> ! {
    use tauri::Listener;

    let result = build(move |app| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
        let app = app.clone();
        std::thread::Builder::new()
            .name("mt-setup-probe".to_string())
            .spawn(move || {
                let (tx, rx) = std::sync::mpsc::channel::<String>();
                app.listen("mt://setup-progress", move |event| {
                    let _ = tx.send(event.payload().to_string());
                });

                let started = if target == "openvino" {
                    setup::start_openvino(app.clone())
                } else {
                    setup::start_model(app.clone(), target.clone())
                };
                if let Err(error) = started {
                    println!("start -> ERR {error}");
                    std::process::exit(1);
                }

                // Progress is chatty by design; only the shape of it is
                // interesting here, so print one line per percent.
                let mut last = -1i64;
                while let Ok(payload) = rx.recv() {
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                        continue;
                    };
                    let phase = value["phase"].as_str().unwrap_or("");
                    let received = value["received"].as_u64().unwrap_or(0);
                    let total = value["total"].as_u64().unwrap_or(0);
                    match phase {
                        "download" | "extract" => {
                            let percent = if total > 0 {
                                (received as i64 * 100) / total as i64
                            } else {
                                -1
                            };
                            if percent != last {
                                last = percent;
                                println!(
                                    "{phase}: {percent}% ({received} / {total}) files {} / {}",
                                    value["filesDone"], value["filesTotal"]
                                );
                            }
                        }
                        "done" => {
                            println!("done: {}", value["message"]);
                            println!("VERDICT: setup path works.");
                            std::process::exit(0);
                        }
                        other => {
                            println!("{other}: {}", value["message"]);
                            println!("VERDICT: FAILED");
                            std::process::exit(1);
                        }
                    }
                }
                println!("VERDICT: FAILED (progress channel closed)");
                std::process::exit(1);
            })
            .expect("プローブスレッドを起動できません");
    });

    if let Err(error) = result {
        eprintln!("--setup を実行できません: {error}");
    }
    std::process::exit(1);
}

/// `--update-check`: drive the updater from a console and exit.
///
/// Runs inside a real app because the updater reads its configuration and its
/// public key from the Tauri context. The window is hidden rather than skipped:
/// it is declared in `tauri.conf.json`, so it exists before `setup` runs.
pub fn run_update_probe(options: update::ProbeOptions) -> ! {
    let result = build(move |app| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
        let app = app.clone();
        std::thread::Builder::new()
            .name("mt-update-probe".to_string())
            .spawn(move || {
                let (status, report) = tauri::async_runtime::block_on(update::probe(&app, &options));
                update::write_report(&report);
                print!("{report}");
                // Leaves the event loop where it stands, on purpose: a probe has
                // no transcript to flush, and the exit code is the result.
                std::process::exit(status);
            })
            .expect("プローブスレッドを起動できません");
    });

    if let Err(error) = result {
        eprintln!("--update-check を実行できません: {error}");
    }
    std::process::exit(1);
}
