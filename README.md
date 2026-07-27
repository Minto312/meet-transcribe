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

## Requirements

- **Windows 11** (WASAPI loopback; the `live`/`capture_probe` binaries are Windows-only)
- **Intel Core Ultra** (Meteor Lake or newer) with the **Intel AI Boost** NPU,
  driver **32.0.100.3104+** — verified on **32.0.100.4724**
- **OpenVINO GenAI runtime 2025.2+** (verified on **2026.2.1**) — download the
  **GenAI** archive, not the plain runtime archive; see
  [notes/environment.md](notes/environment.md#which-openvino-archive-to-download)
- **Rust** (stable, MSVC toolchain) + **VS Build Tools** (VCTools workload)
- For the GUI: **WebView2**, which ships with Windows 11. Nothing else — the
  front end is static files with no build step, so **no Node toolchain**.
- No Python required.

## Quick start

```powershell
# 1. Point the build at OpenVINO (once per shell)
& "C:\path\to\openvino_genai_windows_<ver>_x86_64\setupvars.bat"

# 2. Get a model — official OpenVINO conversions only (see the warning below)
.\scripts\get_model.ps1 -Repo OpenVINO/whisper-medium-int8-ov

# 3a. The GUI
cargo build --release -p meet-transcribe-app
.\target\release\meet-transcribe-app.exe

# 3b. …or the CLI
cargo build --release
cargo run --release --bin live -- --model models\whisper-medium-int8-ov --device NPU
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

Just run the exe. It finds the OpenVINO install itself — `INTEL_OPENVINO_DIR`,
then the usual places an archive gets extracted to (`C:\ov-genai`, `C:\openvino`,
`C:\Program Files\Intel`, next to the exe, …) — and prepends the same
directories to `PATH` that `setupvars.bat` would.

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

### Where transcripts go

`%APPDATA%\com.minto312.meet-transcribe\sessions\<timestamp>.jsonl`, one
append-only file per run: a `meta` line, then one line per utterance, then an
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
  app/ui/                  static front end — no bundler, no Node
scripts/                   setup + measurement (PowerShell) — see scripts/README.md
notes/                     benchmark results and environment facts
export_model.py            optional: convert a model yourself with optimum-intel
reference-python/          the Python prototype this was ported from, kept for
                           cross-checking against OpenVINO's Python API
```

`cargo build --release` at the root builds only the core package and its CLI
binaries; the GUI is `cargo build --release -p meet-transcribe-app`.

## License

MIT — see [LICENSE](LICENSE).
