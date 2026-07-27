# meet-transcribe

**Real-time Japanese transcription of Windows desktop audio (Google Meet, Zoom, video)
on an Intel NPU — written in Rust.**

Captures the system mix via WASAPI loopback, segments it with Silero VAD, and runs
Whisper through OpenVINO GenAI on the **NPU**, the **iGPU**, or the **CPU** — the
device is a single flag.

The point is not raw speed. On a thin laptop the iGPU is already busy compositing the
desktop and decoding video, so the useful question is **which accelerator still keeps
up when the iGPU is saturated**. Measured on a Core Ultra 7 155H, the answer is clear:

| Device | RTF idle | **RTF while the iGPU is saturated** | Degradation |
|---|---|---|---|
| **NPU** | 0.324 | **0.285** | **none** |
| iGPU (Arc) | 0.246 | 0.476 | **1.9× worse** |
| CPU | 0.998 | — | no headroom to begin with |

*RTF = compute time ÷ audio duration; below 1.0 keeps up with live audio.
whisper-medium-int8, 37 s of Japanese speech.*

**The NPU is unaffected by iGPU contention.** The iGPU is marginally faster when the
machine is idle and loses that lead under realistic conditions — while also being the
resource you were trying to protect.

Full numbers and method: **[notes/benchmark-results.md](notes/benchmark-results.md)**.
Hardware, driver versions, and Windows gotchas: **[notes/environment.md](notes/environment.md)**.

> Everything above was measured on real hardware, not estimated. Each figure was taken
> with NPU/iGPU utilization counters sampled during the run, because selecting
> `device="NPU"` is *not* by itself proof the NPU ran the work.

---

## What you get

| binary | purpose |
|---|---|
| **`meet-transcribe-app`** | **The GUI** — model / device / audio-source pickers, start & stop, and every transcript kept in a sidebar |
| `live` | Transcribe desktop audio continuously, printing each utterance with its RTF |
| `bench` | Measure load/compile time and RTF per device (`--devices NPU GPU CPU`) |
| `diag` | Bisect an OpenVINO exception (audio length / language config / device) |
| `capture_probe` | Verify WASAPI loopback capture on its own, independent of ASR |
| `engine_probe` | Drive the GUI's engine from a console — start, capture, stop, drain — with no window |

```
[WASAPI]  a render endpoint opened with Direction::Capture = the system mix
 (src/capture.rs)  (no Stereo Mix, no virtual audio cable), or a microphone
        │          endpoint. WASAPI autoconvert delivers 16 kHz float;
        │          channels are downmixed to mono.
        ▼
[VAD]   Silero VAD via ONNX Runtime, no PyTorch          (src/vad.rs)
        │  utterance ends after ~0.5 s of silence; <0.7 s fragments dropped
        ▼
[ASR]   OpenVINO GenAI WhisperPipeline, device = NPU|GPU|CPU   (src/asr.rs)
        ▼
[out]   "( 2.8s RTF=0.10) 本日の定例ミーティングを始めます"
```

`src/engine.rs` wraps that pipeline in the threading a window needs (see
[The GUI](#the-gui)); `live` still drives the pieces directly in one loop.

## Install

Grab **`meet-transcribe_<version>_x64-setup.exe`** from
[Releases](https://github.com/Minto312/meet-transcribe/releases/latest) and run
it. It installs per-user — no administrator prompt — and adds a Start menu
entry. From then on the app updates itself (see [Updates](#updates)).

The installer is a few MB because it contains only the app. The two large
dependencies are fetched on first launch, by the app, into its own data
directory:

| | size | from |
|---|---|---|
| OpenVINO GenAI runtime | 230 MB download → 222 MB on disk | Intel's official storage |
| a Whisper model | 49 MB (tiny) … 784 MB (medium) | Hugging Face |

That split is deliberate. Bundling them would put a quarter of a gigabyte into
**every** update, and the model is a choice — see
[First run](#first-run-what-the-app-downloads).

## Requirements

To **run** the installed app:

- **Windows 11** (WASAPI loopback), and **WebView2**, which ships with it
- **Intel Core Ultra** (Meteor Lake or newer) with the **Intel AI Boost** NPU,
  driver **32.0.100.3104+** — verified on **32.0.100.4724**. Without an NPU the
  app still runs on the iGPU or the CPU.
- Nothing else: no OpenVINO install, no Python, no Node.

To **build** it:

- **Rust** (stable, MSVC toolchain) + **VS Build Tools** (VCTools workload)
- **OpenVINO GenAI runtime 2025.2+** (verified on **2026.2.1**) — the
  **GenAI** archive, not the plain runtime archive, because
  `openvino-genai-sys` links against `openvino_genai.lib`; see
  [notes/environment.md](notes/environment.md#which-openvino-archive-to-download)
- **Tauri CLI** (`cargo install tauri-cli --locked --version "^2"`) — only to
  build the installer, not for `cargo build`
- Still no Node: the front end is static files with no build step.

## Quick start

```powershell
# 1. Point the build at OpenVINO (once per shell)
& "C:\path\to\openvino_genai_windows_<ver>_x86_64\setupvars.bat"

# 2. Get a model — official OpenVINO conversions only (see the warning below).
#    Or skip this and let the GUI's setup panel do it.
.\scripts\get_model.ps1 -Repo OpenVINO/whisper-medium-int8-ov

# 3a. The GUI
cargo build --release -p meet-transcribe-app
.\target\release\meet-transcribe-app.exe

# 3b. …or the CLI
cargo build --release
cargo run --release --bin live -- --model models\whisper-medium-int8-ov --device NPU

# 3c. …or the installer
.\scripts\release.ps1
```

`live` prints `[1/3] … [3/3]` as it compiles the model, initializes the VAD, and
opens the loopback device. **The first step is slow on the NPU** (see below) — it is
compiling, not hung. The GUI shows the same thing as a *モデル読み込み中* state and
stays responsive throughout.

`live --list-devices` prints the selectable audio endpoints and their ids;
`--mic` and `--audio-device <id>` pick one, matching what the GUI offers.

To reproduce the benchmark:

```powershell
.\scripts\make_sample_wav.ps1                 # Japanese test clip via Windows TTS
.\scripts\prove_npu.ps1 -Device NPU           # RTF + proof the NPU ran it
.\scripts\load_test.ps1  -Device NPU          # under iGPU saturation
.\scripts\load_test.ps1  -Device GPU          # ... and compare
```

## The GUI

A [Tauri](https://tauri.app/) window over the same pipeline: pick a model, pick
`NPU`/`GPU`/`CPU`, pick which audio endpoint to listen to, press start. Every run
is saved automatically and stays in a sidebar.

Just run the exe. It finds the OpenVINO install itself — its own download first,
then `INTEL_OPENVINO_DIR`, then the usual places an archive gets extracted to
(`C:\ov-genai`, `C:\openvino`, `C:\Program Files\Intel`, next to the exe, …) —
and prepends the same directories to `PATH` that `setupvars.bat` would.

That is only possible because `openvino_genai_c.dll` is linked with
`/DELAYLOAD`. With an ordinary load-time import the Windows loader resolves it
*before* `main()` runs, so a machine without OpenVINO on `PATH` gets
`0xC00004BC` and a bare "DLL が見つかりません" dialog, with no opportunity for
the app to say anything. Delaying the import buys the app a chance to fix its
own environment first — and if it genuinely cannot find the runtime, to open
the window and explain instead of vanishing.

`scripts/run_gui.ps1` is still there for an install somewhere unusual; so is
the OpenVINO folder field in the settings dialog.

The half of this that a screenshot cannot check is whether the *delayed* import
resolves when it is finally called — press 開始 and either it works or the delay
stub raises a structured exception and the process disappears. So the binary can
press that button for itself:

```powershell
.\meet-transcribe-app.exe --selftest <model-dir> CPU   # exit 0 = the import resolves
```

### How it is put together

The CLI can afford one blocking loop; a window cannot, because
`WhisperPipeline::new` takes minutes on the NPU. `src/engine.rs` therefore splits
the pipeline across threads:

```
control thread ──select──┬── EngineCommand   (from the UI)
                         └── Notice          (from the workers)
      │ spawns
      ├── capture thread   WASAPI -> Silero VAD -> utterances
      │      per run; nothing in `wasapi` is Send, so the device is opened
      │      on the thread that reads it
      └── ASR thread       long-lived; owns the compiled pipeline and KEEPS IT
                           across stop/start
```

Three consequences worth knowing:

- **A stop/start of the same model+device is instant.** The compiled pipeline is
  cached in the ASR thread, so the ~170 s is paid once per app run, not per
  recording. *モデルを先読み* pays it before the meeting starts.
- **Utterances captured during the compile are not lost.** `Load` and `Work`
  share one FIFO channel, so queued audio is simply transcribed once the model
  lands. The channel is *bounded* at 128 utterances: if the device cannot keep
  up, the UI says how many were dropped instead of growing until the process
  dies.
- **Stopping drains rather than truncates.** The run stays in *残りを処理中*
  until every captured utterance has been through the ASR, then closes.

### Where things go

Transcripts and settings are in the **roaming** profile, where they have always
been; the downloads are hundreds of megabytes of machine-specific binaries and
go in the **local** one, which is also what keeps them out of every update.

```
%APPDATA%\com.minto312.meet-transcribe\
  settings.json
  sessions\<timestamp>.jsonl
%LOCALAPPDATA%\com.minto312.meet-transcribe\
  openvino\openvino_genai_windows_<ver>_x86_64\runtime\...
  models\whisper-medium-int8-ov\...
```

One append-only file per run: a `meta` line, then one line per utterance, then an
`end` line. Each utterance is on disk before it appears in the window, so a
crash costs nothing; a file with no `end` line is just an interrupted session and
reads back fine. *書き出し* renders one as Markdown, *コピー* puts the same on the
clipboard. Runs that captured nothing are discarded rather than left in the
sidebar.

Settings live next to it in `settings.json`.

### What the model picker already knows

Two of this project's most expensive lessons are invisible until `generate()`
throws minutes into a run, so the picker checks for them up front: a directory
containing `openvino_decoder_with_past_model.*` is greyed out as the old
stateless export, and a `large*` model warns that the NPU will reject it. Both
are explained in the next section.

### Deliberate choices

- **`/DELAYLOAD` on the one OpenVINO import**, rather than requiring a launcher
  script or bundling ~212 MB of Intel runtime next to the exe.
- **No Content-Security-Policy** (`"csp": null`). Every asset is compiled into
  the exe and the app loads nothing remote, so a CSP would only add a class of
  first-run failures that are hard to diagnose without a desktop session.
- **Loopback *or* microphone, not both at once.** Mixing two WASAPI streams
  needs clock-drift handling between endpoints; picking one is honest and works.
- **The runtime and the models are downloaded, not bundled.** See
  [First run](#first-run-what-the-app-downloads).
- **The updater is driven from Rust**, not from the plugin's JS bindings. The
  window is a view; one place that talks to the network is easier to verify than
  two, and it is what makes `--update-check` possible on a machine with no
  desktop.

## First run: what the app downloads

The window opens straight into a setup panel whenever the runtime or a model is
missing, and a banner keeps saying so until both are there. Two steps:

1. **OpenVINO GenAI runtime.** The pinned archive (`2026.2.1.0`) is fetched from
   `storage.openvinotoolkit.org`, and **24 of its 1448 entries** are unpacked:
   `runtime/bin/intel64/Release/` and `runtime/3rdparty/tbb/bin/`, minus the
   `*_debug.dll`. That is 222 MB instead of the 873 MB a full extraction would
   cost — everything else in the archive is headers, import libraries, cmake
   files, Python bindings and samples, which matter only when *building* against
   OpenVINO. The zip is deleted afterwards.

   Two things are checked rather than assumed, both of them
   [recorded traps](notes/environment.md#which-openvino-archive-to-download): the
   archive's exact byte count (the storage host answers **HTTP 200 with an HTML
   page** for a path that does not exist, so a wrong URL would otherwise surface
   much later as a corrupt zip), and that
   `runtime\bin\intel64\Release\openvino_genai_c.dll` exists once unpacked.

   No restart is needed afterwards. `openvino::prepare` runs a second time, which
   prepends the new directories to `PATH` and loads the DLL to prove the
   delay-loaded import can now resolve.

2. **A model.** The picker offers the official conversions with their real sizes
   and what each costs on the NPU, `medium` marked as recommended because it is
   the largest one the NPU runs. Files are fetched from the Hugging Face API the
   same way `scripts/get_model.ps1` does it, skipped when already present at the
   expected size, so an interrupted download resumes at file granularity.

Both run on one worker thread at a time, report progress as
`mt://setup-progress`, and can be cancelled. A model lands in whatever folder the
model picker points at, so keeping models on another drive does not produce two
copies.

Neither is something a screenshot can check either, so both can be run from a
console — through the same commands and the same event stream the window uses:

```powershell
.\meet-transcribe-app.exe --setup openvino
.\meet-transcribe-app.exe --setup OpenVINO/whisper-tiny-int8-ov
```

## Updates

The app checks its release feed a few seconds after launch, and again whenever
*更新を確認* is pressed in the settings dialog. When something newer exists, a
banner offers it; accepting it downloads the installer, verifies its signature,
and hands over to NSIS, which replaces the files and relaunches the app.

```
plugins.updater.endpoints
  └─ https://github.com/Minto312/meet-transcribe/releases/latest/download/latest.json
       └─ platforms."windows-x86_64".url  -> meet-transcribe_<ver>_x64-setup.exe
                                signature -> minisign, verified before install
```

The signature is not decoration: the public key is compiled into the binary and
the payload is rejected outright if it does not match, so the update path cannot
be redirected by whoever controls the network. Being offline is *not* treated as
an error — a laptop is offline all the time, and the failed check goes to the log
panel rather than to a red banner.

Nothing in `%LOCALAPPDATA%` is touched by an update, so the 222 MB runtime and the
784 MB model survive it. That is the whole reason they are not bundled.

### Verifying the update path without a desktop

The interesting half of an updater is what happens *after* the check, and none of
it is visible in a screenshot. So the binary can drive itself:

```powershell
# is the feed reachable and does latest.json parse?
.\meet-transcribe-app.exe --update-check

# download it and verify the signature, even if it is not newer than what is
# running -- this is what proves the signing key matches the compiled-in pubkey
.\meet-transcribe-app.exe --update-check --force

# ... and actually run the installer
.\meet-transcribe-app.exe --update-check --force --apply

# ... against a feed that is not the configured one
.\meet-transcribe-app.exe --update-check --endpoint https://example/latest.json
```

The report goes to `%TEMP%\meet-transcribe-update-check.txt` as well as stdout,
because a release build is `windows_subsystem = "windows"` and has nowhere to
print. Exit code 0 means every stage it attempted worked.

## Releasing

```powershell
# once per machine: the signing key. Losing it means no more updates for
# anyone who already installed, so back it up somewhere that is not this repo.
cargo tauri signer generate --ci -w $env:USERPROFILE\.tauri\meet-transcribe.key
# put the contents of ...\meet-transcribe.key.pub into
# tauri.conf.json > plugins > updater > pubkey

# per release: bump the version in tauri.conf.json AND both Cargo.toml files
# (release.ps1 refuses to build if they disagree), then
.\scripts\release.ps1
gh release create v0.2.0 --title v0.2.0 --notes-file NOTES.md `
  target\release\bundle\nsis\meet-transcribe_0.2.0_x64-setup.exe `
  target\release\bundle\nsis\latest.json
```

`release.ps1` builds the NSIS bundle with `cargo tauri build --ci`, which signs
the installer because `bundle.createUpdaterArtifacts` is true, and then writes the
`latest.json` the updater expects. The `releases/latest/download/` URL always
resolves to the newest non-prerelease, so uploading those two files *is* the
publish step.

`--ci` matters twice: the signing key has no passphrase, and without `--ci` the
bundler prompts for one on stdin — which hangs a non-interactive session forever.

## Things that will bite you

Each of these cost hours to find; they are the main reason this repo exists.

**Use the official `OpenVINO/*` models. Community `*-ov-npu` builds fail.**
`FluidInference/whisper-large-v3-turbo-int4-ov-npu` constructs fine and then throws
`unknown exception` from `generate()` **on every device**, even for 5 s of audio with
default settings. It ships `openvino_decoder_with_past_model.*` — the older
**stateless** export, which OpenVINO GenAI 2026.x will not run. The official
conversions are stateful and work.

**`medium` is the largest model that runs on the NPU.**
`large-v3-turbo` constructs but throws from `generate()` on the NPU (matching
[openvino.genai #1965](https://github.com/openvinotoolkit/openvino.genai/issues/1965)).
It does work on the iGPU. tiny / base / small / medium are all fine on the NPU.

**NPU compilation is slow and is not cached.**
Every process start pays it: **small ≈ 60 s, medium ≈ 170 s**. A second run of the
same model is no faster, so the driver's UMD cache is not helping here — consistent
with [openvino.genai #1992](https://github.com/openvinotoolkit/openvino.genai/issues/1992)
("CACHE_DIR not being used with NPU"). The Rust `WhisperPipeline::new(path, device)`
also has no way to pass `ov::cache_dir`. **Design for a long-running process**; the
cost is paid once at startup. `bench` prints `load/compile: N s` separately from RTF.

**Language must be a token, not a bare code.**
`set_language("<|ja|>")` works; `set_language("ja")` throws.

**Whisper hallucinates on near-silence.** Short blips reliably produced
「ご視聴ありがとうございました」. `src/vad.rs` drops segments under 0.7 s, which
removes almost all of it.

## Status

Built, run, and measured on real hardware (2026-07-27) — see
[notes/benchmark-results.md](notes/benchmark-results.md). Working: NPU/iGPU/CPU
inference with counter-verified device attribution, WASAPI loopback and
microphone capture, and the full `live` pipeline.

Rough edges, honestly stated:

- Startup latency on the NPU (above) — the biggest usability problem. The GUI
  hides it behind a preload button and a resident pipeline, but it is still
  minutes the first time.
- Utterance-level latency only. Text appears when an utterance ends; there are no
  streaming partials yet (LocalAgreement-2 is the intended fix).
- The benchmark clip is clean single-speaker TTS. Real meetings — overlapping
  speakers, noise, disfluencies — are harder, so treat the transcription quality here
  as optimistic. The RTF comparison between devices is unaffected.
- Occasional hallucination on trailing silence.
- Loopback captures the whole system mix, so it picks up other applications'
  sound. Isolating one process needs `wasapi`'s `new_application_loopback_client`.
- Loopback and microphone are alternatives, not a mix: you cannot record both
  sides of a call into one transcript yet.
- No speaker diarization — the transcript is a flat list of utterances.
- The GUI now starts without OpenVINO on `PATH` and constructs a real pipeline
  through the delay-loaded import (`--selftest`, verified on the reference
  machine with `INTEL_OPENVINO_DIR` unset).
- The GUI's *engine* is verified end-to-end on the reference machine
  (`scripts/test_engine.ps1`: NPU compile 97 s, five utterances at RTF
  0.17–0.38, clean drain back to idle), and the front end was exercised against
  a mocked backend in a browser. The assembled window has been launched and
  confirmed to run, but has not been clicked through by hand — the reference
  machine is driven over SSH, which has no interactive desktop.

### What the installer and the updater were tested against

All on the reference machine (2026-07-27), over SSH, with no desktop session:

| | result |
|---|---|
| `scripts/release.ps1` | 8.6 MB signed installer + `.sig` + `latest.json` |
| silent install (`/S`) | per-user in `%LOCALAPPDATA%\meet-transcribe`, `DisplayVersion 0.2.0`, Start menu shortcut, `uninstall.exe`, no elevation |
| `--setup OpenVINO/whisper-tiny-int8-ov` | 20 files / 48.8 MB; a second run skips what is already there |
| `--setup openvino` | 229,626,351 B fetched, 24 files / 222,513,665 B unpacked, no `*_debug.dll`, the zip deleted afterwards |
| `--selftest` through that runtime | pipeline constructs on CPU — so the 24-file subset is genuinely sufficient |
| update check, feed not newer | reports no update |
| update check, feed newer | finds it without `--force` |
| download + signature | 9,015,780 B, verified against the compiled-in public key |
| tampered signature | rejected before anything is installed, exit 1 |
| install | hands over to NSIS and exits 0 |

Two gaps, stated rather than papered over: the installer's `/R` relaunch after a
self-update was not observed (nothing can launch a window on that machine), and
an upgrade *between two different versions* has not been run — only a reinstall
of the same one over itself.

## Layout

```
src/asr.rs                 OpenVINO WhisperPipeline wrapper (device switch + timing)
src/capture.rs             WASAPI capture: loopback or microphone, + enumeration
src/vad.rs                 Silero VAD streaming segmentation
src/engine.rs              threaded orchestration behind the GUI (model cache,
                           bounded queue, run state machine)
src/audio.rs               downmix / resample helpers
src/bin/{live,bench,diag,capture_probe,engine_probe}.rs
app/                       the Tauri GUI (its own package in the workspace)
  app/src/                 state, engine->webview bridge, commands, session store
  app/src/openvino.rs      locating the runtime at startup (see the GUI section)
  app/src/setup.rs         downloading the runtime and the models on first run
  app/src/update.rs        checking, verifying and installing updates
  app/ui/                  static front end — no bundler, no Node
scripts/                   setup + measurement (PowerShell) — see scripts/README.md
scripts/release.ps1        the signed installer + the updater manifest
notes/                     benchmark results and environment facts
export_model.py            optional: convert a model yourself with optimum-intel
reference-python/          the Python prototype this was ported from, kept for
                           cross-checking against OpenVINO's Python API
```

`cargo build --release` at the root builds only the core package and its CLI
binaries; the GUI is `cargo build --release -p meet-transcribe-app`.

## License

MIT — see [LICENSE](LICENSE).
