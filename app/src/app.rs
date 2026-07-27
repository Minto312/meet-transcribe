//! Tauri wiring: shared state, the engine→webview bridge, and app startup.

use std::path::PathBuf;
use std::sync::Mutex;

use crossbeam_channel::Receiver;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use meet_transcribe::engine::{
    Engine, EngineCommand, EngineEvent, EngineStatus, LogRecord, Phase,
};

use crate::models;
use crate::openvino::OpenvinoStatus;
use crate::store::{SegmentRecord, SessionStore, Settings};

pub struct AppState {
    pub engine: Engine,
    pub store: SessionStore,
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    /// Whether the OpenVINO runtime was found at startup. Decided once, before
    /// any thread exists, because resolving it mutates the process PATH.
    pub openvino: Mutex<OpenvinoStatus>,
    /// Mirror of the last status pushed by the engine, so a freshly opened or
    /// reloaded window can ask for it instead of waiting for the next change.
    pub status: Mutex<EngineStatus>,
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
                                LogRecord::error(format!("セッションの保存に失敗しました: {e:#}")),
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
                        LogRecord::error(format!("文字起こしの保存に失敗しました: {e:#}")),
                    );
                }
            },
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let settings_path = data_dir.join("settings.json");
            let mut settings = Settings::load(&settings_path);
            if settings.models_dir.is_none() {
                settings.models_dir = models::guess_models_dir();
            }
            let _ = settings.save(&settings_path);

            let store = SessionStore::new(data_dir.join("sessions"))?;

            // Before any thread exists: this prepends to the process PATH so the
            // delay-loaded OpenVINO import can resolve when the ASR thread first
            // needs it.
            let openvino = crate::openvino::prepare(settings.openvino_dir.as_deref());

            let (engine, events) = Engine::spawn();

            app.manage(AppState {
                engine,
                store,
                settings: Mutex::new(settings),
                settings_path,
                openvino: Mutex::new(openvino),
                status: Mutex::new(EngineStatus::default()),
            });

            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("mt-bridge".to_string())
                .spawn(move || bridge(handle, events))?;

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
        ])
        .run(tauri::generate_context!())
        .expect("meet-transcribe GUI の起動に失敗しました");
}
