//! Settings and the transcript archive.
//!
//! Sessions are append-only JSONL, one file per run:
//!
//! ```text
//! {"t":"meta","id":"20260727-101530",...}      exactly one, first line
//! {"t":"seg","index":0,"text":"..."}           one per transcribed utterance
//! {"t":"title","title":"定例MTG"}               zero or more, last one wins
//! {"t":"end","endedAt":"...","segments":42}    zero or one
//! ```
//!
//! Append-only matters: every utterance is fsync-free but already on disk the
//! moment it appears in the window, so killing the app (or a crash three
//! minutes into an NPU compile) never loses a transcript. A file with no `end`
//! record is simply an interrupted session, and reads back fine.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use meet_transcribe::capture::{DeviceKind, DeviceSelection};
use meet_transcribe::engine::{
    SegmentEvent, DEFAULT_MAX_SEGMENT_S, DEFAULT_MIN_SILENCE_MS, DEFAULT_VAD_THRESHOLD,
};

// ------------------------------------------------------------------- settings

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Directory scanned for OpenVINO IR model folders.
    pub models_dir: Option<PathBuf>,
    /// Root of the OpenVINO GenAI install, when it is somewhere the app cannot
    /// guess. Normally `None`: the usual locations are searched at startup.
    pub openvino_dir: Option<PathBuf>,
    /// Name of the selected model folder inside `models_dir`.
    pub model: Option<String>,
    /// `NPU` | `GPU` | `CPU`
    pub device: String,
    pub language: String,
    pub audio: DeviceSelection,
    pub max_segment_s: f32,
    pub vad_threshold: f32,
    pub min_silence_ms: u32,
    pub auto_scroll: bool,
    /// Show the per-utterance duration / RTF badge.
    pub show_metrics: bool,
    /// Compile the selected model as soon as the app starts. Paired with
    /// starting at sign-in, the NPU's ~170 s is over before the meeting.
    pub preload_on_launch: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            models_dir: None,
            openvino_dir: None,
            model: None,
            device: "NPU".to_string(),
            language: "ja".to_string(),
            audio: DeviceSelection::default_loopback(),
            max_segment_s: DEFAULT_MAX_SEGMENT_S,
            vad_threshold: DEFAULT_VAD_THRESHOLD,
            min_silence_ms: DEFAULT_MIN_SILENCE_MS,
            auto_scroll: true,
            show_metrics: true,
            preload_on_launch: true,
        }
    }
}

impl Settings {
    /// Never fails: a corrupt or missing settings file falls back to defaults
    /// rather than stopping the app from opening.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).with_context(|| format!("write {}", path.display()))
    }
}

// -------------------------------------------------------------- on-disk records

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    pub started_at: DateTime<Local>,
    pub model: String,
    pub device: String,
    pub language: String,
    pub audio_device: String,
    pub audio_kind: DeviceKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentRecord {
    pub index: u64,
    /// Seconds from the start of capture.
    pub offset_s: f64,
    pub audio_s: f64,
    pub rtf: f64,
    /// Wall-clock time the utterance began.
    pub at: DateTime<Local>,
    pub text: String,
    // Everything below is absent from transcripts written before v0.3.0.
    /// Seconds the utterance queued before the device took it. With `rtf` it
    /// gives how late the text appeared, which `at` cannot: `at` is derived
    /// from captured samples, and WASAPI delivers none while nothing plays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_s: Option<f64>,
    /// Tokens decoded, and the cap they were decoded under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<usize>,
    /// Characters of a repetition loop removed from `text`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeats_removed: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
enum Record {
    #[serde(rename = "meta")]
    Meta(SessionMeta),
    #[serde(rename = "seg")]
    Seg(SegmentRecord),
    #[serde(rename = "title")]
    Title { title: String },
    #[serde(rename = "end")]
    End {
        #[serde(rename = "endedAt")]
        ended_at: DateTime<Local>,
        segments: u64,
        #[serde(rename = "audioSeconds")]
        audio_seconds: f64,
    },
}

// ------------------------------------------------------------------ UI views

/// Sidebar row.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub started_at: DateTime<Local>,
    pub ended_at: Option<DateTime<Local>>,
    pub segment_count: usize,
    pub audio_seconds: f64,
    pub preview: String,
    pub model: String,
    pub device: String,
    pub audio_device: String,
    pub audio_kind: DeviceKind,
    pub language: String,
    /// True when the file has no `end` record: the app was closed mid-run.
    pub interrupted: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    pub summary: SessionSummary,
    pub segments: Vec<SegmentRecord>,
}

// ------------------------------------------------------------------- the store

struct ActiveSession {
    id: String,
    started_at: DateTime<Local>,
    file: File,
    segments: u64,
    audio_seconds: f64,
}

pub struct SessionStore {
    dir: PathBuf,
    active: Mutex<Option<ActiveSession>>,
}

impl SessionStore {
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(Self {
            dir,
            active: Mutex::new(None),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Session ids double as filenames, and they arrive back from the frontend,
    /// so anything that is not a plain timestamp-ish token is rejected outright.
    fn path_for(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("不正なセッション ID です: {id}");
        }
        Ok(self.dir.join(format!("{id}.jsonl")))
    }

    /// Start recording. Returns the new session id.
    pub fn begin(
        &self,
        started_at: DateTime<Local>,
        model: String,
        device: String,
        language: String,
        audio_device: String,
        audio_kind: DeviceKind,
    ) -> Result<String> {
        let mut active = self.active.lock().unwrap();
        if active.is_some() {
            bail!("すでに記録中のセッションがあります");
        }

        // Second resolution is enough (a run cannot start twice in one second),
        // but collide-proof it anyway rather than silently appending to a file.
        let base = started_at.format("%Y%m%d-%H%M%S").to_string();
        let (id, path) = (0..100)
            .map(|n| {
                if n == 0 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .filter_map(|id| self.path_for(&id).ok().map(|p| (id, p)))
            .find(|(_, path)| !path.exists())
            .ok_or_else(|| anyhow!("セッションファイル名を決められませんでした"))?;

        let mut file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;

        let meta = SessionMeta {
            id: id.clone(),
            started_at,
            model,
            device,
            language,
            audio_device,
            audio_kind,
        };
        writeln!(file, "{}", serde_json::to_string(&Record::Meta(meta))?)?;
        file.flush()?;

        *active = Some(ActiveSession {
            id: id.clone(),
            started_at,
            file,
            segments: 0,
            audio_seconds: 0.0,
        });
        Ok(id)
    }

    pub fn active_id(&self) -> Option<String> {
        self.active.lock().unwrap().as_ref().map(|a| a.id.clone())
    }

    /// Persist one transcribed utterance. Returns the stored record and the
    /// session it belongs to, or `None` when nothing is being recorded.
    pub fn append(&self, event: &SegmentEvent) -> Result<Option<(String, SegmentRecord)>> {
        let mut guard = self.active.lock().unwrap();
        let Some(active) = guard.as_mut() else {
            return Ok(None);
        };

        let at = active.started_at
            + chrono::Duration::milliseconds((event.offset_s * 1000.0).round() as i64);
        let record = SegmentRecord {
            index: event.index,
            offset_s: event.offset_s,
            audio_s: event.audio_s,
            rtf: event.rtf,
            at,
            text: event.text.clone(),
            wait_s: Some(event.wait_s),
            tokens: event.tokens,
            max_tokens: event.max_tokens,
            repeats_removed: event.repeats_removed,
        };
        writeln!(
            active.file,
            "{}",
            serde_json::to_string(&Record::Seg(record.clone()))?
        )?;
        active.file.flush()?;
        active.segments += 1;
        active.audio_seconds += event.audio_s;
        Ok(Some((active.id.clone(), record)))
    }

    /// Close the active session. Returns its id and how many utterances it
    /// captured, so the caller can discard runs that produced nothing.
    pub fn finish(&self) -> Result<Option<(String, u64)>> {
        let mut guard = self.active.lock().unwrap();
        let Some(mut active) = guard.take() else {
            return Ok(None);
        };
        let record = Record::End {
            ended_at: Local::now(),
            segments: active.segments,
            audio_seconds: active.audio_seconds,
        };
        writeln!(active.file, "{}", serde_json::to_string(&record)?)?;
        active.file.flush()?;
        Ok(Some((active.id, active.segments)))
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir).with_context(|| format!("read {}", self.dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // A single unreadable file must not hide the whole archive.
            if let Ok(detail) = read_session(&path) {
                out.push(detail.summary);
            }
        }
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    }

    pub fn load(&self, id: &str) -> Result<SessionDetail> {
        read_session(&self.path_for(id)?)
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<()> {
        let path = self.path_for(id)?;
        if !path.exists() {
            bail!("セッションが見つかりません: {id}");
        }
        let title = title.trim();
        if title.is_empty() {
            bail!("タイトルが空です");
        }
        let mut file = OpenOptions::new().append(true).open(&path)?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(&Record::Title {
                title: title.to_string()
            })?
        )?;
        file.flush()?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        if self.active_id().as_deref() == Some(id) {
            bail!("記録中のセッションは削除できません");
        }
        let path = self.path_for(id)?;
        fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))
    }

    pub fn markdown(&self, id: &str) -> Result<String> {
        Ok(to_markdown(&self.load(id)?))
    }
}

fn read_session(path: &Path) -> Result<SessionDetail> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut meta: Option<SessionMeta> = None;
    let mut segments: Vec<SegmentRecord> = Vec::new();
    let mut title: Option<String> = None;
    let mut ended_at: Option<DateTime<Local>> = None;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        // A truncated last line (killed mid-write) is expected, not an error.
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            continue;
        };
        match record {
            Record::Meta(m) => meta = Some(m),
            Record::Seg(s) => segments.push(s),
            Record::Title { title: t } => title = Some(t),
            Record::End { ended_at: e, .. } => ended_at = Some(e),
        }
    }

    let meta = meta.ok_or_else(|| anyhow!("メタ行がありません: {}", path.display()))?;
    let audio_seconds = segments.iter().map(|s| s.audio_s).sum();
    let preview: String = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect();

    let title = title.unwrap_or_else(|| derive_title(&meta, &segments));

    Ok(SessionDetail {
        summary: SessionSummary {
            id: meta.id,
            title,
            started_at: meta.started_at,
            ended_at,
            segment_count: segments.len(),
            audio_seconds,
            preview,
            model: meta.model,
            device: meta.device,
            audio_device: meta.audio_device,
            audio_kind: meta.audio_kind,
            language: meta.language,
            interrupted: ended_at.is_none(),
        },
        segments,
    })
}

/// Untitled sessions are named after their first line, the way a chat is named
/// after its first message.
fn derive_title(meta: &SessionMeta, segments: &[SegmentRecord]) -> String {
    match segments.first() {
        Some(first) => {
            let text: String = first.text.chars().take(28).collect();
            if first.text.chars().count() > 28 {
                format!("{text}…")
            } else {
                text
            }
        }
        None => format!("{} の記録", meta.started_at.format("%m/%d %H:%M")),
    }
}

fn to_markdown(detail: &SessionDetail) -> String {
    let s = &detail.summary;
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", s.title));
    out.push_str(&format!(
        "- 開始: {}\n",
        s.started_at.format("%Y-%m-%d %H:%M:%S")
    ));
    if let Some(ended) = s.ended_at {
        out.push_str(&format!("- 終了: {}\n", ended.format("%Y-%m-%d %H:%M:%S")));
    }
    out.push_str(&format!("- モデル: {} ({})\n", s.model, s.device));
    out.push_str(&format!(
        "- 音声: {} [{}]\n",
        s.audio_device,
        s.audio_kind.label()
    ));
    out.push_str(&format!(
        "- 発話数: {} / 音声 {:.1} 秒\n\n",
        s.segment_count, s.audio_seconds
    ));
    if s.interrupted {
        out.push_str("> このセッションは正常終了せずに中断されました。\n\n");
    }
    out.push_str("---\n\n");
    for segment in &detail.segments {
        out.push_str(&format!(
            "**{}**  {}\n\n",
            segment.at.format("%H:%M:%S"),
            segment.text
        ));
    }
    out
}
