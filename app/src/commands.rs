//! Everything the webview can call. Each returns `Result<_, String>` so a
//! failure shows up as a rejected promise with a readable Japanese message
//! rather than a silent no-op.

use std::path::PathBuf;

use chrono::Local;
use serde::{Deserialize, Serialize};
use tauri::State;

use meet_transcribe::capture::{list_devices, AudioDeviceInfo, DeviceSelection};
use meet_transcribe::engine::{EngineCommand, EngineStatus, ModelSpec, RunConfig};

use crate::app::AppState;
use crate::models::{self, ModelInfo};
use crate::openvino::{self, OpenvinoStatus};
use crate::store::{SessionDetail, SessionSummary, Settings};

fn to_message(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListing {
    pub dir: Option<String>,
    pub models: Vec<ModelInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    pub model_dir: String,
    pub device: String,
    pub language: String,
}

impl ModelRequest {
    fn into_spec(self) -> Result<ModelSpec, String> {
        let dir = PathBuf::from(&self.model_dir);
        if !dir.is_dir() {
            return Err(format!("モデルフォルダが見つかりません: {}", dir.display()));
        }
        Ok(ModelSpec {
            dir,
            device: self.device,
            language: self.language,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRequest {
    pub model_dir: String,
    /// Display name recorded with the transcript.
    pub model_name: String,
    pub device: String,
    pub language: String,
    pub audio: DeviceSelection,
    /// Display name of the endpoint, recorded with the transcript. The engine
    /// discovers the real name when it opens the device, but the archive should
    /// say what was asked for even if the open fails.
    pub audio_device_name: String,
    pub max_segment_s: f32,
    pub vad_threshold: f32,
    pub min_silence_ms: u32,
}

// ------------------------------------------------------------------- settings

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
pub fn set_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    *state.settings.lock().unwrap() = settings;
    state.persist_settings().map_err(to_message)
}

// ------------------------------------------------------------------ openvino

#[tauri::command]
pub fn openvino_status(state: State<'_, AppState>) -> OpenvinoStatus {
    state.openvino.lock().unwrap().clone()
}

/// Record where OpenVINO lives. Takes effect on the next launch: the runtime is
/// resolved once, before any thread exists, and re-pointing `PATH` underneath a
/// process that may already have loaded a different copy is not something to do
/// while the app is running.
#[tauri::command]
pub fn set_openvino_dir(state: State<'_, AppState>, dir: String) -> Result<(), String> {
    let path = PathBuf::from(&dir);
    openvino::check_root(&path)?;
    state.settings.lock().unwrap().openvino_dir = Some(path);
    state.persist_settings().map_err(to_message)
}

// --------------------------------------------------------------------- audio

#[tauri::command]
pub fn list_audio_devices() -> Result<Vec<AudioDeviceInfo>, String> {
    // COM is thread-affine and nothing in `wasapi` is Send, so enumeration runs
    // on a scratch thread that dies with its COM apartment instead of leaving
    // one initialized on a Tauri worker.
    std::thread::spawn(list_devices)
        .join()
        .map_err(|_| "オーディオデバイスの列挙がパニックしました".to_string())?
        .map_err(to_message)
}

// -------------------------------------------------------------------- models

#[tauri::command]
pub fn list_models(state: State<'_, AppState>) -> ModelListing {
    let dir = state.settings.lock().unwrap().models_dir.clone();
    match dir {
        Some(dir) => ModelListing {
            models: models::scan(&dir),
            dir: Some(dir.display().to_string()),
        },
        None => ModelListing {
            dir: None,
            models: Vec::new(),
        },
    }
}

#[tauri::command]
pub fn set_models_dir(state: State<'_, AppState>, dir: String) -> Result<ModelListing, String> {
    let path = PathBuf::from(&dir);
    if !path.is_dir() {
        return Err(format!("フォルダが見つかりません: {dir}"));
    }
    state.settings.lock().unwrap().models_dir = Some(path.clone());
    state.persist_settings().map_err(to_message)?;
    Ok(ModelListing {
        models: models::scan(&path),
        dir: Some(path.display().to_string()),
    })
}

// -------------------------------------------------------------------- engine

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> EngineStatus {
    state.status.lock().unwrap().clone()
}

/// Refuse to touch the ASR path when the runtime never resolved. A delay-loaded
/// import that cannot be satisfied raises a structured exception and kills the
/// process without a word, so this guard is what turns that into a message.
fn require_openvino(state: &State<'_, AppState>) -> Result<(), String> {
    match &*state.openvino.lock().unwrap() {
        OpenvinoStatus::Ready { .. } => Ok(()),
        OpenvinoStatus::Missing { message, .. } => Err(message.clone()),
    }
}

#[tauri::command]
pub fn preload_model(state: State<'_, AppState>, req: ModelRequest) -> Result<(), String> {
    require_openvino(&state)?;
    state.engine.send(EngineCommand::Preload(req.into_spec()?));
    Ok(())
}

#[tauri::command]
pub fn start_transcription(
    state: State<'_, AppState>,
    req: StartRequest,
) -> Result<String, String> {
    require_openvino(&state)?;
    if state.store.active_id().is_some() {
        return Err("すでに記録中です".to_string());
    }
    let dir = PathBuf::from(&req.model_dir);
    if !dir.is_dir() {
        return Err(format!("モデルフォルダが見つかりません: {}", dir.display()));
    }

    // Open the transcript first: if this fails there is no point capturing.
    let session_id = state
        .store
        .begin(
            Local::now(),
            req.model_name.clone(),
            req.device.clone(),
            req.language.clone(),
            req.audio_device_name.clone(),
            req.audio.kind,
        )
        .map_err(to_message)?;

    state.engine.send(EngineCommand::Start(Box::new(RunConfig {
        model: ModelSpec {
            dir,
            device: req.device,
            language: req.language,
        },
        audio: req.audio,
        max_segment_s: req.max_segment_s,
        vad_threshold: req.vad_threshold,
        min_silence_ms: req.min_silence_ms,
    })));

    Ok(session_id)
}

#[tauri::command]
pub fn stop_transcription(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.send(EngineCommand::Stop);
    Ok(())
}

// ------------------------------------------------------------------ sessions

#[tauri::command]
pub fn active_session(state: State<'_, AppState>) -> Option<String> {
    state.store.active_id()
}

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionSummary>, String> {
    state.store.list().map_err(to_message)
}

#[tauri::command]
pub fn get_session(state: State<'_, AppState>, id: String) -> Result<SessionDetail, String> {
    state.store.load(&id).map_err(to_message)
}

#[tauri::command]
pub fn rename_session(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> Result<(), String> {
    state.store.rename(&id, &title).map_err(to_message)
}

#[tauri::command]
pub fn delete_session(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.delete(&id).map_err(to_message)
}

#[tauri::command]
pub fn session_markdown(state: State<'_, AppState>, id: String) -> Result<String, String> {
    state.store.markdown(&id).map_err(to_message)
}

#[tauri::command]
pub fn save_session_as(
    state: State<'_, AppState>,
    id: String,
    path: String,
) -> Result<(), String> {
    let markdown = state.store.markdown(&id).map_err(to_message)?;
    std::fs::write(&path, markdown).map_err(|e| format!("{path} への書き込みに失敗しました: {e}"))
}

#[tauri::command]
pub fn sessions_dir(state: State<'_, AppState>) -> String {
    state.store.dir().display().to_string()
}
