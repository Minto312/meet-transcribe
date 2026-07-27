//! In-app updates.
//!
//! The release feed is a `latest.json` attached to the newest GitHub release
//! (`scripts/release.ps1` writes it), the payload is the NSIS installer, and the
//! signature is minisign — the updater refuses anything not signed by the key
//! whose public half is compiled into `tauri.conf.json`.
//!
//! Driven from Rust rather than from the plugin's JS bindings, for the same
//! reason everything else here is: the window is a view, and one place that
//! knows how to talk to the outside world is easier to verify than two. It also
//! means the update path can be exercised **without a desktop**, which matters
//! because the reference machine is driven over SSH:
//!
//! ```powershell
//! meet-transcribe-app.exe --update-check --force --apply
//! ```
//!
//! `--force` accepts the newest release even when it is the version already
//! running, so check → download → *signature verification* can be proven against
//! a real release without publishing a throwaway one. See `probe`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use meet_transcribe::engine::LogRecord;

use crate::app::AppState;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    /// Release notes from the feed, if the release had a body.
    pub notes: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    version: String,
    received: u64,
    total: u64,
    /// `download` | `install` | `error`
    phase: &'static str,
    message: Option<String>,
}

/// The version the running binary reports — the one `tauri.conf.json` was built
/// with, which is also what the updater compares against the feed.
pub fn current_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Ask the feed whether there is something newer.
///
/// `endpoint` and `force` exist for the console probe; the UI passes neither.
async fn check_with(
    app: &AppHandle,
    endpoint: Option<&str>,
    force: bool,
) -> Result<Option<Update>, String> {
    let mut builder = app.updater_builder();
    if let Some(endpoint) = endpoint {
        let url = endpoint
            .parse()
            .map_err(|e| format!("更新の取得先 URL が不正です ({endpoint}): {e}"))?;
        builder = builder
            .endpoints(vec![url])
            .map_err(|e| format!("更新の取得先を設定できません: {e}"))?;
    }
    if force {
        // Deliberately accepts an equal version: this is how the download and
        // signature check get exercised against an already-published release.
        builder = builder.version_comparator(|_current, _release| true);
    }
    builder
        .build()
        .map_err(|e| format!("更新の確認を準備できません: {e}"))?
        .check()
        .await
        .map_err(|e| format!("更新を確認できませんでした: {e}"))
}

fn info(update: &Update) -> UpdateInfo {
    UpdateInfo {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
    }
}

/// Check, and remember the result so a later `install` does not have to re-ask.
pub async fn check(app: &AppHandle) -> Result<Option<UpdateInfo>, String> {
    let found = check_with(app, None, false).await?;
    let described = found.as_ref().map(info);
    *app.state::<AppState>().pending_update.lock().unwrap() = found;
    Ok(described)
}

/// Download the update, verify its signature, and hand over to the installer.
///
/// On Windows this does not return on success: `Update::install` starts the NSIS
/// installer and calls `exit(0)`, and the installer relaunches the app once it
/// has replaced the files (`/R`, with `installMode: passive`).
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let update = app
        .state::<AppState>()
        .pending_update
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "更新の情報がありません。先に更新を確認してください。".to_string())?;

    let version = update.version.clone();
    let mut progress = DownloadProgress {
        version: version.clone(),
        received: 0,
        total: 0,
        phase: "download",
        message: None,
    };
    let _ = app.emit("mt://update-progress", progress.clone());

    let mut last = Instant::now() - Duration::from_secs(1);
    let mut received = 0u64;
    let result = update
        .download_and_install(
            |chunk, total| {
                received += chunk as u64;
                if last.elapsed() >= Duration::from_millis(120) {
                    last = Instant::now();
                    progress.received = received;
                    progress.total = total.unwrap_or(0);
                    let _ = app.emit("mt://update-progress", progress.clone());
                }
            },
            || {
                let _ = app.emit(
                    "mt://update-progress",
                    DownloadProgress {
                        version: version.clone(),
                        received: 0,
                        total: 0,
                        phase: "install",
                        message: Some("インストーラーを起動します…".to_string()),
                    },
                );
            },
        )
        .await;

    if let Err(error) = result {
        let message = format!("更新のインストールに失敗しました: {error}");
        let _ = app.emit(
            "mt://update-progress",
            DownloadProgress {
                version: update.version.clone(),
                received: 0,
                total: 0,
                phase: "error",
                message: Some(message.clone()),
            },
        );
        let _ = app.emit("mt://log", LogRecord::error(message.clone()));
        return Err(message);
    }
    Ok(())
}

/// Look for an update shortly after startup and tell the window if there is one.
///
/// A plain thread rather than an async task: the delay is the point (the first
/// seconds after launch belong to the model list and the audio device
/// enumeration), and everything else in this app already waits on threads.
pub fn spawn_startup_check(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("mt-update-check".to_string())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(4));
            match tauri::async_runtime::block_on(check(&app)) {
                Ok(Some(found)) => {
                    let _ = app.emit("mt://update-available", found.clone());
                    let _ = app.emit(
                        "mt://log",
                        LogRecord::info(format!(
                            "新しいバージョン {} が公開されています (現在 {})。",
                            found.version, found.current_version
                        )),
                    );
                }
                Ok(None) => {}
                // Being offline is the normal case for a laptop, not an error
                // worth a red banner. It stays in the log.
                Err(error) => {
                    let _ = app.emit("mt://log", LogRecord::warn(error));
                }
            }
        });
}

// ---------------------------------------------------------------------- probe

pub struct ProbeOptions {
    /// Override the feed URL. Non-https is rejected in release builds.
    pub endpoint: Option<String>,
    /// Accept a release that is not newer than the running version.
    pub force: bool,
    /// Actually download, verify and run the installer.
    pub apply: bool,
}

/// `--update-check`: exercise the update path from a console and report.
///
/// Returns the exit code and the report. What each stage proves:
///
/// - **check** — the endpoint is reachable and `latest.json` parses.
/// - **download** — the payload URL is right and, because `download` verifies
///   before returning, the signature matches the compiled-in public key. This is
///   the half that a screenshot of the settings dialog can never establish.
/// - **apply** — the NSIS installer actually starts (the process exits here).
pub async fn probe(app: &AppHandle, options: &ProbeOptions) -> (i32, String) {
    use std::fmt::Write as _;
    let mut report = String::new();

    let _ = writeln!(report, "current version: {}", current_version(app));
    let _ = writeln!(
        report,
        "endpoint: {}",
        options.endpoint.as_deref().unwrap_or("(tauri.conf.json)")
    );
    let _ = writeln!(report, "force: {}  apply: {}", options.force, options.apply);

    let found = match check_with(app, options.endpoint.as_deref(), options.force).await {
        Ok(found) => found,
        Err(error) => {
            let _ = writeln!(report, "check -> ERR {error}");
            let _ = writeln!(report, "VERDICT: FAILED");
            return (1, report);
        }
    };

    let Some(update) = found else {
        let _ = writeln!(report, "check -> no update (feed is not newer)");
        let _ = writeln!(report, "VERDICT: reachable, nothing to install.");
        return (0, report);
    };

    let _ = writeln!(
        report,
        "check -> {} available (from {})",
        update.version, update.download_url
    );
    if !options.apply {
        let _ = writeln!(
            report,
            "VERDICT: feed parsed. Re-run with --apply to download and install."
        );
        return (0, report);
    }

    // `download` verifies the minisign signature before it returns, so reaching
    // the next line is proof the key in tauri.conf.json matches the signing key.
    let bytes = match update.download(|_, _| {}, || {}).await {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = writeln!(report, "download -> ERR {error}");
            let _ = writeln!(report, "VERDICT: FAILED");
            return (1, report);
        }
    };
    let _ = writeln!(
        report,
        "download -> {} bytes, signature verified against the compiled-in pubkey",
        bytes.len()
    );

    // Writes the report before handing over: `install` never returns.
    let _ = writeln!(
        report,
        "install -> starting the NSIS installer (this process exits now)"
    );
    let _ = writeln!(report, "VERDICT: update path works end to end.");
    write_report(&report);
    print!("{report}");

    if let Err(error) = update.install(bytes) {
        let mut tail = report.clone();
        let _ = writeln!(tail, "install -> ERR {error}");
        let _ = writeln!(tail, "VERDICT: FAILED");
        return (1, tail);
    }
    (0, report)
}

/// A GUI-subsystem binary has no stdout to print to in a release build, so the
/// report goes to a file as well — same trick as `--selftest`.
pub fn write_report(report: &str) {
    let out = std::path::PathBuf::from(std::env::var("TEMP").unwrap_or_else(|_| ".".into()))
        .join("meet-transcribe-update-check.txt");
    let _ = std::fs::write(out, report);
}

/// Somewhere to keep the result of a check between the two commands.
pub type Pending = Mutex<Option<Update>>;
