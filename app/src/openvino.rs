//! Finding the OpenVINO runtime at startup, so the app does not need a launcher.
//!
//! `openvino_genai_c.dll` is linked with `/DELAYLOAD` (see `build.rs`), which
//! buys us the one thing a load-time import cannot give: a chance to run before
//! the DLL has to resolve. This module uses that chance to reproduce what
//! `setupvars.bat` does — prepend the OpenVINO bin directories to `PATH` — and
//! then proves it worked by loading the DLL itself.
//!
//! Proving matters. If the delayed import cannot be resolved later, the delay
//! stub raises a structured exception and the process dies with no message. So
//! we load it eagerly here, keep the handle, and if that fails we say so in the
//! window instead of letting the user press 開始 into a crash.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The one OpenVINO DLL this binary imports directly.
const DLL: &str = "openvino_genai_c.dll";

/// Directories `setupvars.bat` prepends to `PATH`, relative to the install root.
/// Taken from the script itself (it builds `OPENVINO_LIB_PATHS` from these);
/// the Debug variant is skipped because a release install does not ship it.
const BIN_DIRS: &[&str] = &[
    r"runtime\3rdparty\tbb\bin",
    r"runtime\bin\intel64\Release",
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum OpenvinoStatus {
    /// The runtime resolved. `root` is empty when it was already on `PATH`
    /// (i.e. the app was launched through `scripts/run_gui.ps1`).
    Ready { root: String },
    /// Nothing usable was found. The window still opens; transcription cannot start.
    Missing { message: String, searched: Vec<String> },
}

/// A directory is an OpenVINO install if it holds the DLL we import.
fn is_install_root(root: &Path) -> bool {
    root.join(r"runtime\bin\intel64\Release").join(DLL).is_file()
}

/// Every place worth looking, best guess first.
fn candidate_roots(hint: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !roots.contains(&path) {
            roots.push(path);
        }
    };

    // 1. Whatever the user pointed at in settings.
    if let Some(hint) = hint {
        push(hint.to_path_buf());
    }
    // 2. Set by a previous setupvars.bat run. It ends with a separator, which
    //    `PathBuf` copes with but makes for ugly display, so normalise it.
    if let Ok(dir) = std::env::var("INTEL_OPENVINO_DIR") {
        let trimmed = dir.trim_end_matches(['\\', '/']);
        if !trimmed.is_empty() {
            push(PathBuf::from(trimmed));
        }
    }

    // 3. The usual places an archive gets extracted to. Each base is tried both
    //    as an install root and as a parent of versioned subdirectories, since
    //    the archive unpacks to openvino_genai_windows_<version>_x86_64/.
    let mut bases: Vec<PathBuf> = vec![
        PathBuf::from(r"C:\ov-genai"),
        PathBuf::from(r"C:\openvino"),
        PathBuf::from(r"C:\Program Files\Intel"),
        PathBuf::from(r"C:\Program Files (x86)\Intel"),
        PathBuf::from(r"C:\Intel"),
    ];
    for var in ["USERPROFILE", "LOCALAPPDATA"] {
        if let Ok(dir) = std::env::var(var) {
            bases.push(PathBuf::from(&dir));
            bases.push(PathBuf::from(&dir).join("Programs"));
        }
    }
    // Also next to the exe, for anyone who unpacks the runtime beside the app.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            bases.push(dir.to_path_buf());
        }
    }

    for base in bases {
        push(base.clone());
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        let mut versioned: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.to_ascii_lowercase().starts_with("openvino"))
            })
            .collect();
        // Newest-looking install first: the names carry the version.
        versioned.sort();
        versioned.reverse();
        for path in versioned {
            push(path);
        }
    }

    roots
}

/// Prepend `dirs` to this process's `PATH`, the way setupvars.bat does.
fn prepend_to_path(root: &Path) {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut joined: Vec<PathBuf> = BIN_DIRS
        .iter()
        .map(|dir| root.join(dir))
        .filter(|dir| dir.is_dir())
        .collect();
    joined.extend(std::env::split_paths(&existing));
    if let Ok(path) = std::env::join_paths(joined) {
        // Safe here: called before any other thread is spawned, and the Windows
        // loader reads PATH from the process environment block on each search.
        std::env::set_var("PATH", path);
    }
}

/// Load the DLL and keep it resident, so the delayed import resolves to an
/// already-loaded module later instead of searching again.
fn load_dll() -> Result<(), String> {
    // SAFETY: loading a DLL runs its DllMain. This is Intel's own runtime,
    // the same one the process would load implicitly a moment later.
    match unsafe { libloading::Library::new(DLL) } {
        Ok(library) => {
            // Dropping the handle would FreeLibrary it straight back out.
            std::mem::forget(library);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Make `openvino_genai_c.dll` loadable, or explain why it is not.
///
/// Call this before spawning anything: it mutates the process environment.
pub fn prepare(hint: Option<&Path>) -> OpenvinoStatus {
    // Launched through run_gui.ps1, or OpenVINO is on the system PATH already.
    if load_dll().is_ok() {
        return OpenvinoStatus::Ready {
            root: std::env::var("INTEL_OPENVINO_DIR").unwrap_or_else(|_| "PATH".to_string()),
        };
    }

    let candidates = candidate_roots(hint);
    for root in &candidates {
        if !is_install_root(root) {
            continue;
        }
        prepend_to_path(root);
        if load_dll().is_ok() {
            return OpenvinoStatus::Ready {
                root: root.display().to_string(),
            };
        }
    }

    OpenvinoStatus::Missing {
        message: format!(
            "OpenVINO GenAI ランタイムが見つかりません ({DLL} を読み込めませんでした)。\
             GenAI アーカイブ (openvino_genai_windows_<version>_x86_64) を展開し、\
             設定でそのフォルダを指定してください。"
        ),
        searched: candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
    }
}

/// Validate a folder the user picked in the settings dialog.
pub fn check_root(root: &Path) -> Result<(), String> {
    if is_install_root(root) {
        return Ok(());
    }
    Err(format!(
        "{} は OpenVINO のインストール先ではないようです \
         (runtime\\bin\\intel64\\Release\\{DLL} がありません)。\
         GenAI アーカイブを展開したフォルダを指定してください。",
        root.display()
    ))
}
