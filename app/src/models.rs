//! Discovering usable OpenVINO IR Whisper models on disk.
//!
//! The model picker does more than list folders, because two of this project's
//! most expensive lessons are invisible until `generate()` throws minutes into
//! a run:
//!
//! - a directory containing `openvino_decoder_with_past_model.*` is the older
//!   **stateless** export, which OpenVINO GenAI 2026.x will not run *on any
//!   device* (this is what the community `*-ov-npu` builds ship);
//! - `large-v3-turbo` constructs fine on the NPU and then throws from
//!   `generate()` — `medium` is the largest model the NPU actually runs.
//!
//! Both are surfaced here as warnings on the entry, so the UI can grey out the
//! first and caution about the second instead of letting you find out later.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Files an OpenVINO GenAI `WhisperPipeline` directory must contain.
const REQUIRED: &[&str] = &[
    "openvino_encoder_model.xml",
    "openvino_decoder_model.xml",
    "openvino_tokenizer.xml",
    "openvino_detokenizer.xml",
];

/// The marker of the old stateless export.
const STATELESS_MARKER: &str = "openvino_decoder_with_past_model.xml";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Directory name, which is how the model is labelled everywhere.
    pub name: String,
    pub path: String,
    /// False when the directory cannot work at all.
    pub usable: bool,
    /// Blocking reasons (empty when `usable`).
    pub problems: Vec<String>,
    /// Non-blocking caveats, e.g. "will not run on the NPU".
    pub warnings: Vec<String>,
    /// Devices this model is not expected to work on.
    pub unsupported_devices: Vec<String>,
    pub size_mb: u64,
}

/// List every subdirectory of `dir` that looks like a model, usable or not.
/// Unusable ones are still returned so the UI can explain *why* rather than
/// silently omitting a folder the user can plainly see.
pub fn scan(dir: &Path) -> Vec<ModelInfo> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out: Vec<ModelInfo> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| inspect(&path))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Whether `dir` holds a model a `WhisperPipeline` would accept.
///
/// Used by the setup panel to decide what is already downloaded, and by the
/// downloader to refuse to call a half-finished directory a model.
pub fn is_usable(dir: &Path) -> bool {
    inspect(dir).is_some_and(|info| info.usable)
}

fn inspect(path: &Path) -> Option<ModelInfo> {
    let name = path.file_name()?.to_string_lossy().into_owned();

    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|f| !path.join(f).is_file())
        .collect();
    // Not a model directory at all -- don't clutter the list with it.
    if missing.len() == REQUIRED.len() {
        return None;
    }

    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    let mut unsupported_devices = Vec::new();

    if !missing.is_empty() {
        problems.push(format!("必要なファイルがありません: {}", missing.join(", ")));
    }
    if path.join(STATELESS_MARKER).is_file() {
        problems.push(
            "旧 stateless 形式です (openvino_decoder_with_past_model.* を含む)。\
             OpenVINO GenAI 2026.x では generate() が例外になります。\
             公式の OpenVINO/whisper-*-ov を使ってください。"
                .to_string(),
        );
    }
    // Name-based, and labelled as such: there is no way to tell from the IR
    // whether the NPU will accept it short of compiling for minutes.
    if name.contains("large") {
        warnings.push(
            "large 系はモデル名から判断して NPU では generate() が例外になります \
             (このマシンで確認済み。NPU は medium が上限)。GPU / CPU では動作します。"
                .to_string(),
        );
        unsupported_devices.push("NPU".to_string());
    }

    let size_mb = fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum::<u64>()
                / (1024 * 1024)
        })
        .unwrap_or(0);

    Some(ModelInfo {
        name,
        path: path.display().to_string(),
        usable: problems.is_empty(),
        problems,
        warnings,
        unsupported_devices,
        size_mb,
    })
}

/// Best guess at where `models/` lives, used the first time the app runs.
///
/// The exe normally sits at `<repo>/target/release/`, so walking up a few
/// levels finds the repository's own `models/` without the user pointing at it.
pub fn guess_models_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..4 {
            let Some(current) = dir else { break };
            candidates.push(current.join("models"));
            dir = current.parent().map(Path::to_path_buf);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("models"));
    }

    candidates.into_iter().find(|path| path.is_dir())
}
