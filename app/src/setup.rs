//! First-run setup: fetching the two things the installer deliberately does not
//! ship.
//!
//! The exe is a few MB. What it needs in order to actually transcribe is not:
//! the OpenVINO GenAI runtime is a 230 MB archive and a usable Whisper model is
//! 49 MB to 784 MB. Bundling them would mean re-downloading a quarter of a
//! gigabyte on every app update, so the app fetches them once, into its own data
//! directory, and the updater only ever ships the binary.
//!
//! ```text
//! %LOCALAPPDATA%\com.minto312.meet-transcribe\
//!   openvino\openvino_genai_windows_<ver>_x86_64\runtime\...   <- 24 files, 222 MB
//!   models\whisper-medium-int8-ov\...                          <- from Hugging Face
//!   cache\openvino-genai-<ver>.zip                             <- transient
//! ```
//!
//! One download runs at a time, on its own thread, reporting progress as
//! `mt://setup-progress` events. Nothing here blocks the window: 開始 stays
//! disabled until the runtime lands, which is the same guard a missing
//! hand-installed runtime already goes through.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use meet_transcribe::engine::LogRecord;

use crate::app::AppState;

/// The OpenVINO GenAI archive this project is pinned to.
///
/// Pinned rather than "latest" on purpose. `notes/environment.md` records two
/// traps: the plain `openvino_toolkit_*` archive has no GenAI content at all,
/// and the storage host answers **HTTP 200 with an HTML page** for paths that do
/// not exist, so a wrong version string fails as a corrupt zip rather than as a
/// 404. This is the version the benchmarks were measured on.
const OPENVINO_VERSION: &str = "2026.2.1.0";
const OPENVINO_URL: &str = "https://storage.openvinotoolkit.org/repositories/openvino_genai/packages/2026.2.1/windows/openvino_genai_windows_2026.2.1.0_x86_64.zip";
/// Exact archive size, so a truncated transfer or an HTML error page is caught
/// before it is unpacked. Also gives the UI a total before the first byte lands.
const OPENVINO_ARCHIVE_BYTES: u64 = 229_626_351;

/// Entries worth unpacking, relative to the archive's single top-level directory.
///
/// The archive is 873 MB unpacked, of which the runtime needs 222 MB: these two
/// directories are exactly the ones `setupvars.bat` puts on `PATH`. Everything
/// else is headers, import libraries, cmake files, Python bindings and samples —
/// all of which matter only when *building* against OpenVINO.
const KEEP_PREFIXES: &[&str] = &["runtime/bin/intel64/Release/", "runtime/3rdparty/tbb/bin/"];

/// Marker for "the user pressed 中止", kept out of the error path proper so the
/// UI can distinguish it from a failure.
const CANCELLED: &str = "cancelled";

/// The Whisper conversions offered in the picker.
///
/// Official `OpenVINO/*` conversions only — the community `*-ov-npu` builds ship
/// the old stateless export and throw from `generate()` on every device (see
/// `notes/benchmark-results.md`). Sizes are the sum of each repo's blobs, as
/// reported by the Hugging Face API.
///
/// The notes are kept short deliberately: they share one line with the name, the
/// size and a button, and a note that overflows loses its ellipsis exactly where
/// the useful part is.
const CATALOG: &[Catalog] = &[
    Catalog {
        repo: "OpenVINO/whisper-tiny-int8-ov",
        size_mb: 49,
        note: "最速・最も粗い",
        npu: true,
        recommended: false,
    },
    Catalog {
        repo: "OpenVINO/whisper-base-int8-ov",
        size_mb: 85,
        note: "tiny より少し良い",
        npu: true,
        recommended: false,
    },
    Catalog {
        repo: "OpenVINO/whisper-small-int8-ov",
        size_mb: 257,
        note: "NPU コンパイル約 60 秒",
        npu: true,
        recommended: false,
    },
    Catalog {
        repo: "OpenVINO/whisper-medium-int8-ov",
        size_mb: 784,
        note: "NPU で動く最大。約 170 秒",
        npu: true,
        recommended: true,
    },
    Catalog {
        repo: "OpenVINO/whisper-large-v3-turbo-int4-ov",
        size_mb: 478,
        note: "NPU 不可・GPU / CPU 専用",
        npu: false,
        recommended: false,
    },
];

struct Catalog {
    repo: &'static str,
    size_mb: u64,
    note: &'static str,
    npu: bool,
    recommended: bool,
}

fn repo_name(repo: &str) -> String {
    repo.rsplit('/').next().unwrap_or(repo).to_string()
}

// ---------------------------------------------------------------------- state

/// Where the app keeps what it downloaded.
#[derive(Clone, Debug)]
pub struct Paths {
    /// Parent of the versioned OpenVINO install directory.
    pub openvino: PathBuf,
    pub models: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            openvino: data_dir.join("openvino"),
            models: data_dir.join("models"),
            cache: data_dir.join("cache"),
        }
    }

    /// Where this archive version unpacks to.
    fn openvino_root(&self) -> PathBuf {
        self.openvino
            .join(format!("openvino_genai_windows_{OPENVINO_VERSION}_x86_64"))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// `openvino` | `model`
    pub task: String,
    /// Human-readable name of what is being fetched.
    pub target: String,
}

/// One download at a time, cancellable, with the current job visible to the UI.
pub struct SetupState {
    pub paths: Paths,
    running: Mutex<Option<Job>>,
    cancel: Arc<AtomicBool>,
}

impl SetupState {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            running: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn busy(&self) -> Option<Job> {
        self.running.lock().unwrap().clone()
    }

    /// Claim the worker slot, or explain that it is taken.
    fn claim(&self, job: Job) -> Result<Arc<AtomicBool>, String> {
        let mut slot = self.running.lock().unwrap();
        if let Some(current) = slot.as_ref() {
            return Err(format!("{} のダウンロード中です", current.target));
        }
        self.cancel.store(false, Ordering::SeqCst);
        *slot = Some(job);
        Ok(self.cancel.clone())
    }

    fn release(&self) {
        *self.running.lock().unwrap() = None;
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// --------------------------------------------------------------------- events

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    task: &'static str,
    target: String,
    /// `download` | `extract` | `done` | `error` | `cancelled`
    phase: &'static str,
    received: u64,
    total: u64,
    files_done: u32,
    files_total: u32,
    /// Current file, or the closing message.
    message: Option<String>,
}

impl Progress {
    fn new(task: &'static str, target: &str, phase: &'static str) -> Self {
        Self {
            task,
            target: target.to_string(),
            phase,
            received: 0,
            total: 0,
            files_done: 0,
            files_total: 0,
            message: None,
        }
    }
}

fn emit(app: &AppHandle, progress: &Progress) {
    let _ = app.emit("mt://setup-progress", progress.clone());
}

fn log(app: &AppHandle, record: LogRecord) {
    let _ = app.emit("mt://log", record);
}

/// Report bytes at most ~8 times a second. The webview does not need 900 events
/// for a 230 MB download, and each one costs a JSON round trip.
struct Throttle(Option<Instant>);

impl Throttle {
    fn new() -> Self {
        Self(None)
    }

    fn ready(&mut self) -> bool {
        let now = Instant::now();
        match self.0 {
            Some(last) if now.duration_since(last) < Duration::from_millis(120) => false,
            _ => {
                self.0 = Some(now);
                true
            }
        }
    }
}

// --------------------------------------------------------------------- status

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub repo: String,
    /// Directory name the repo lands in, which is also how the picker labels it.
    pub name: String,
    pub size_mb: u64,
    pub note: String,
    /// False for models known to fail on the NPU.
    pub npu: bool,
    pub recommended: bool,
    pub installed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub openvino_ready: bool,
    /// Set when a runtime was found; it may be an install the app did not create.
    pub openvino_root: Option<String>,
    /// True when the app's own download is present and usable.
    pub openvino_managed: bool,
    pub openvino_dir: String,
    pub openvino_size_mb: u64,
    pub openvino_version: String,
    pub models_dir: Option<String>,
    pub managed_models_dir: String,
    pub catalog: Vec<CatalogEntry>,
    pub busy: Option<Job>,
}

pub fn status(
    setup: &SetupState,
    openvino: &crate::openvino::OpenvinoStatus,
    models_dir: Option<&Path>,
) -> SetupStatus {
    let fallback = setup.paths.models.clone();
    let scan_dir = models_dir.unwrap_or(&fallback);

    SetupStatus {
        openvino_ready: openvino.is_ready(),
        openvino_root: match openvino {
            crate::openvino::OpenvinoStatus::Ready { root } => Some(root.clone()),
            crate::openvino::OpenvinoStatus::Missing { .. } => None,
        },
        openvino_managed: crate::openvino::is_install_root(&setup.paths.openvino_root()),
        openvino_dir: setup.paths.openvino.display().to_string(),
        openvino_size_mb: OPENVINO_ARCHIVE_BYTES / (1024 * 1024),
        openvino_version: OPENVINO_VERSION.to_string(),
        models_dir: models_dir.map(|dir| dir.display().to_string()),
        managed_models_dir: setup.paths.models.display().to_string(),
        catalog: CATALOG
            .iter()
            .map(|entry| {
                let name = repo_name(entry.repo);
                CatalogEntry {
                    repo: entry.repo.to_string(),
                    installed: crate::models::is_usable(&scan_dir.join(&name)),
                    name,
                    size_mb: entry.size_mb,
                    note: entry.note.to_string(),
                    npu: entry.npu,
                    recommended: entry.recommended,
                }
            })
            .collect(),
        busy: setup.busy(),
    }
}

// ------------------------------------------------------------------ downloads

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(concat!("meet-transcribe/", env!("CARGO_PKG_VERSION")))
        // The blocking client defaults to a 30 s *total* timeout, which a 784 MB
        // model download would trip every time. Only the connect phase gets a
        // deadline; a slow link must not be mistaken for a dead one.
        .timeout(None::<Duration>)
        .connect_timeout(Duration::from_secs(30))
        .build()
        .context("HTTP クライアントを作成できません")
}

/// Stream `url` into `dest`, reporting progress and honouring cancellation.
///
/// Writes to a sibling `.part` file and renames on success, so an interrupted
/// download is never mistaken for a complete file.
fn download_to_file(
    http: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, u64),
) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("{} を作成できません", parent.display()))?;
    }
    let mut part = dest.as_os_str().to_os_string();
    part.push(".part");
    let part = PathBuf::from(part);

    let mut response = http
        .get(url)
        .send()
        .with_context(|| format!("{url} に接続できません"))?;
    if !response.status().is_success() {
        bail!("{url} が HTTP {} を返しました", response.status());
    }
    let total = response.content_length().unwrap_or(0);

    let mut file =
        fs::File::create(&part).with_context(|| format!("{} を作成できません", part.display()))?;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut received = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            drop(file);
            let _ = fs::remove_file(&part);
            bail!("{CANCELLED}");
        }
        let read = response
            .read(&mut buffer)
            .with_context(|| format!("{url} の受信中にエラーが発生しました"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("{} への書き込みに失敗しました", part.display()))?;
        received += read as u64;
        on_progress(received, total);
    }
    file.flush()?;
    drop(file);

    if total > 0 && received != total {
        let _ = fs::remove_file(&part);
        bail!("受信バイト数が一致しません ({received} / {total}): 転送が途中で切れました");
    }
    fs::rename(&part, dest)
        .with_context(|| format!("{} を {} に移動できません", part.display(), dest.display()))?;
    Ok(received)
}

/// Download and unpack the OpenVINO GenAI runtime. Returns the install root.
fn install_openvino(
    app: &AppHandle,
    paths: &Paths,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<PathBuf> {
    let http = client()?;
    let archive = paths
        .cache
        .join(format!("openvino-genai-{OPENVINO_VERSION}.zip"));

    // A previous attempt may have got the archive but failed to unpack it. The
    // size check is what makes reusing it safe: a truncated file would otherwise
    // surface much later as a corrupt zip.
    let complete = fs::metadata(&archive)
        .map(|m| m.len() == OPENVINO_ARCHIVE_BYTES)
        .unwrap_or(false);

    if complete {
        log(
            app,
            LogRecord::info(format!(
                "ダウンロード済みのアーカイブを再利用します: {}",
                archive.display()
            )),
        );
    } else {
        progress.phase = "download";
        progress.total = OPENVINO_ARCHIVE_BYTES;
        emit(app, progress);

        let mut throttle = Throttle::new();
        let mut tick = progress.clone();
        download_to_file(&http, OPENVINO_URL, &archive, cancel, |received, total| {
            if throttle.ready() {
                tick.received = received;
                tick.total = if total > 0 {
                    total
                } else {
                    OPENVINO_ARCHIVE_BYTES
                };
                emit(app, &tick);
            }
        })?;

        // The storage host serves an HTML page with HTTP 200 for a path that does
        // not exist, so "we received bytes" is not proof we received the archive.
        let size = fs::metadata(&archive)?.len();
        if size != OPENVINO_ARCHIVE_BYTES {
            let _ = fs::remove_file(&archive);
            bail!(
                "アーカイブのサイズが想定と違います ({size} バイト、期待値 \
                 {OPENVINO_ARCHIVE_BYTES})。配布元が変わった可能性があります: {OPENVINO_URL}"
            );
        }
    }

    progress.phase = "extract";
    progress.received = 0;
    progress.total = 0;
    emit(app, progress);

    let root = extract_runtime(app, &archive, &paths.openvino, cancel, progress)?;

    // 230 MB of zip is worth nothing once unpacked, and the app's data directory
    // is not the place to hoard it.
    let _ = fs::remove_file(&archive);

    if !crate::openvino::is_install_root(&root) {
        bail!(
            "展開後に {} が見つかりません。アーカイブの構造が変わった可能性があります。",
            root.join(r"runtime\bin\intel64\Release\openvino_genai_c.dll")
                .display()
        );
    }
    Ok(root)
}

/// Unpack just the DLLs, preserving the archive's own directory layout.
///
/// Returns the versioned install root — the directory `setupvars.bat` would have
/// been run from, which is exactly what `openvino::prepare` knows how to find.
fn extract_runtime(
    app: &AppHandle,
    archive: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<PathBuf> {
    let file =
        fs::File::open(archive).with_context(|| format!("{} を開けません", archive.display()))?;
    let mut zip = zip::ZipArchive::new(std::io::BufReader::new(file))
        .with_context(|| format!("{} を zip として読めません", archive.display()))?;

    // Decide what to unpack up front, so the UI gets a real total.
    let mut wanted: Vec<(usize, PathBuf, u64)> = Vec::new();
    let mut top: Option<String> = None;
    for index in 0..zip.len() {
        let entry = zip.by_index_raw(index)?;
        if entry.is_dir() {
            continue;
        }
        // Rejects absolute paths and `..`, which is the only protection a zip
        // offers against writing outside the destination.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let Some(name) = path.to_str() else { continue };
        let name = name.replace('\\', "/");
        let Some((first, rest)) = name.split_once('/') else {
            continue;
        };
        if !KEEP_PREFIXES.iter().any(|prefix| rest.starts_with(prefix)) {
            continue;
        }
        // The Debug DLLs double the size and a release build never loads them.
        if rest.ends_with("_debug.dll") {
            continue;
        }
        if top.is_none() {
            top = Some(first.to_string());
        }
        wanted.push((index, PathBuf::from(&name), entry.size()));
    }

    let top = top.ok_or_else(|| {
        anyhow!(
            "{} に runtime ディレクトリが見つかりません",
            archive.display()
        )
    })?;

    progress.files_total = wanted.len() as u32;
    progress.total = wanted.iter().map(|(_, _, size)| size).sum();
    emit(app, progress);

    let mut written = 0u64;
    let mut throttle = Throttle::new();
    for (done, (index, relative, _)) in wanted.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            bail!("{CANCELLED}");
        }
        let out = dest.join(relative);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("{} を作成できません", parent.display()))?;
        }
        let mut entry = zip.by_index(*index)?;
        let mut sink = fs::File::create(&out)
            .with_context(|| format!("{} を作成できません", out.display()))?;
        written += std::io::copy(&mut entry, &mut sink)
            .with_context(|| format!("{} の展開に失敗しました", out.display()))?;

        progress.files_done = done as u32 + 1;
        progress.received = written;
        if throttle.ready() || done + 1 == wanted.len() {
            emit(app, progress);
        }
    }

    Ok(dest.join(top))
}

#[derive(Deserialize)]
struct HfModel {
    siblings: Vec<HfSibling>,
}

#[derive(Deserialize)]
struct HfSibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
}

/// Download one Hugging Face repo into `<models_dir>/<name>/`.
///
/// Mirrors `scripts/get_model.ps1`: list the repo through the API, then fetch
/// each blob. Files already on disk at the expected size are skipped, so an
/// interrupted download resumes at file granularity.
fn install_model(
    app: &AppHandle,
    models_dir: &Path,
    repo: &str,
    cancel: &AtomicBool,
    progress: &mut Progress,
) -> Result<PathBuf> {
    let http = client()?;
    let meta: HfModel = http
        .get(format!("https://huggingface.co/api/models/{repo}?blobs=true"))
        .send()
        .with_context(|| format!("Hugging Face の {repo} に接続できません"))?
        .error_for_status()
        .with_context(|| format!("Hugging Face が {repo} を返しませんでした"))?
        .json()
        .context("Hugging Face の応答を解釈できません")?;

    // `.gitattributes` is git plumbing and `.ipynb_checkpoints/` is notebook
    // litter; neither is part of the model.
    let files: Vec<HfSibling> = meta
        .siblings
        .into_iter()
        .filter(|s| {
            s.rfilename != ".gitattributes" && !s.rfilename.starts_with(".ipynb_checkpoints/")
        })
        .collect();
    if files.is_empty() {
        bail!("{repo} にダウンロードするファイルがありません");
    }

    let dest = models_dir.join(repo_name(repo));
    fs::create_dir_all(&dest).with_context(|| format!("{} を作成できません", dest.display()))?;

    progress.phase = "download";
    progress.total = files.iter().map(|f| f.size.unwrap_or(0)).sum();
    progress.files_total = files.len() as u32;
    emit(app, progress);

    let mut carried = 0u64;
    for (done, file) in files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            bail!("{CANCELLED}");
        }
        let out = dest.join(&file.rfilename);
        let expected = file.size.unwrap_or(0);
        let have = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        if have > 0 && (expected == 0 || have == expected) {
            carried += have;
            progress.files_done = done as u32 + 1;
            progress.received = carried;
            progress.message = Some(file.rfilename.clone());
            emit(app, progress);
            continue;
        }

        let url = format!(
            "https://huggingface.co/{repo}/resolve/main/{}",
            file.rfilename
        );
        let mut throttle = Throttle::new();
        let mut tick = progress.clone();
        tick.message = Some(file.rfilename.clone());
        let base = carried;
        let got = download_to_file(&http, &url, &out, cancel, |received, _| {
            if throttle.ready() {
                tick.received = base + received;
                emit(app, &tick);
            }
        })?;

        carried += got;
        progress.files_done = done as u32 + 1;
        progress.received = carried;
        progress.message = Some(file.rfilename.clone());
        emit(app, progress);
    }

    if !crate::models::is_usable(&dest) {
        bail!(
            "{} は OpenVINO GenAI のモデルとして不完全です。もう一度ダウンロードしてください。",
            dest.display()
        );
    }
    Ok(dest)
}

// -------------------------------------------------------------------- workers

/// Push the outcome to the window, whichever way the job ended.
fn finish(app: &AppHandle, progress: &mut Progress, result: Result<String>) {
    match result {
        Ok(message) => {
            progress.phase = "done";
            progress.message = Some(message.clone());
            log(app, LogRecord::info(message));
        }
        Err(error) if error.to_string() == CANCELLED => {
            progress.phase = CANCELLED;
            progress.message = Some("ダウンロードを中止しました。".to_string());
            log(app, LogRecord::warn("ダウンロードを中止しました。"));
        }
        Err(error) => {
            let message = format!("{error:#}");
            progress.phase = "error";
            progress.message = Some(message.clone());
            log(app, LogRecord::error(message));
        }
    }
    emit(app, progress);
}

/// Start fetching the OpenVINO runtime. Returns as soon as the worker starts.
pub fn start_openvino(app: AppHandle) -> Result<(), String> {
    let label = format!("OpenVINO GenAI {OPENVINO_VERSION}");
    let (cancel, paths) = {
        let state = app.state::<AppState>();

        // Windows locks a loaded DLL, and `prepare` deliberately keeps
        // openvino_genai_c.dll resident, so unpacking over the copy this process
        // is using would fail halfway through and leave a broken install. Only
        // this exact case is refused: unpacking a fresh copy while a *different*
        // install is loaded touches no locked file and is allowed.
        if let crate::openvino::OpenvinoStatus::Ready { root } = &*state.openvino.lock().unwrap() {
            if Path::new(root) == state.setup.paths.openvino_root() {
                return Err(format!(
                    "{} は現在読み込まれているため上書きできません。\
                     取得し直すにはアプリを再起動してから実行してください。",
                    root
                ));
            }
        }

        let cancel = state.setup.claim(Job {
            task: "openvino".to_string(),
            target: label.clone(),
        })?;
        (cancel, state.setup.paths.clone())
    };

    std::thread::Builder::new()
        .name("mt-setup-openvino".to_string())
        .spawn(move || {
            let mut progress = Progress::new("openvino", &label, "download");
            let outcome = install_openvino(&app, &paths, &cancel, &mut progress);

            let state = app.state::<AppState>();
            let result = match outcome {
                Ok(root) => {
                    // Re-resolve so the delay-loaded import can be satisfied
                    // without restarting: this prepends the new bin directories
                    // to PATH and loads the DLL to prove it worked.
                    let hint = state.settings.lock().unwrap().openvino_dir.clone();
                    let status = crate::openvino::prepare(hint.as_deref(), Some(&paths.openvino));
                    let ready = status.is_ready();
                    *state.openvino.lock().unwrap() = status.clone();
                    let _ = app.emit("mt://openvino-status", status);
                    if ready {
                        Ok(format!(
                            "OpenVINO GenAI {OPENVINO_VERSION} を {} に用意しました。",
                            root.display()
                        ))
                    } else {
                        Err(anyhow!(
                            "{} に展開しましたが openvino_genai_c.dll を読み込めませんでした。",
                            root.display()
                        ))
                    }
                }
                Err(error) => Err(error),
            };
            finish(&app, &mut progress, result);
            state.setup.release();
        })
        .map_err(|e| format!("ダウンロードスレッドを起動できません: {e}"))?;
    Ok(())
}

/// Start fetching one model from the catalog.
pub fn start_model(app: AppHandle, repo: String) -> Result<(), String> {
    if !CATALOG.iter().any(|entry| entry.repo == repo) {
        return Err(format!("未知のモデルです: {repo}"));
    }
    let name = repo_name(&repo);

    let (cancel, models_dir) = {
        let state = app.state::<AppState>();
        let cancel = state.setup.claim(Job {
            task: "model".to_string(),
            target: name.clone(),
        })?;
        // Models land in whatever folder the picker is pointed at, so somebody
        // keeping models on another drive does not end up with two copies.
        let models_dir = state
            .settings
            .lock()
            .unwrap()
            .models_dir
            .clone()
            .unwrap_or_else(|| state.setup.paths.models.clone());
        (cancel, models_dir)
    };

    std::thread::Builder::new()
        .name("mt-setup-model".to_string())
        .spawn(move || {
            let mut progress = Progress::new("model", &name, "download");
            let result = install_model(&app, &models_dir, &repo, &cancel, &mut progress)
                .map(|dir| format!("{} を {} に用意しました。", name, dir.display()));
            finish(&app, &mut progress, result);
            app.state::<AppState>().setup.release();
        })
        .map_err(|e| format!("ダウンロードスレッドを起動できません: {e}"))?;
    Ok(())
}
