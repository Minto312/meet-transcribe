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
| `live` | Transcribe desktop audio continuously, printing each utterance with its RTF |
| `bench` | Measure load/compile time and RTF per device (`--devices NPU GPU CPU`) |
| `diag` | Bisect an OpenVINO exception (audio length / language config / device) |
| `capture_probe` | Verify WASAPI loopback capture on its own, independent of ASR |

```
[WASAPI loopback]  default render device opened with Direction::Capture
   (src/capture.rs)  = the system mix. No Stereo Mix, no virtual audio cable.
        │            WASAPI autoconvert delivers 16 kHz float; channels downmixed
        ▼
[VAD]   Silero VAD via ONNX Runtime, no PyTorch          (src/vad.rs)
        │  utterance ends after ~0.5 s of silence; <0.7 s fragments dropped
        ▼
[ASR]   OpenVINO GenAI WhisperPipeline, device = NPU|GPU|CPU   (src/asr.rs)
        ▼
[out]   "( 2.8s RTF=0.10) 本日の定例ミーティングを始めます"
```

## Requirements

- **Windows 11** (WASAPI loopback; the `live`/`capture_probe` binaries are Windows-only)
- **Intel Core Ultra** (Meteor Lake or newer) with the **Intel AI Boost** NPU,
  driver **32.0.100.3104+** — verified on **32.0.100.4724**
- **OpenVINO GenAI runtime 2025.2+** (verified on **2026.2.1**) — download the
  **GenAI** archive, not the plain runtime archive; see
  [notes/environment.md](notes/environment.md#which-openvino-archive-to-download)
- **Rust** (stable, MSVC toolchain) + **VS Build Tools** (VCTools workload)
- No Python required.

## Quick start

```powershell
# 1. Point the build at OpenVINO (once per shell)
& "C:\path\to\openvino_genai_windows_<ver>_x86_64\setupvars.bat"

# 2. Get a model — official OpenVINO conversions only (see the warning below)
.\scripts\get_model.ps1 -Repo OpenVINO/whisper-medium-int8-ov

# 3. Build
cargo build --release

# 4. Transcribe whatever your speakers are playing
cargo run --release --bin live -- --model models\whisper-medium-int8-ov --device NPU
```

`live` prints `[1/3] … [3/3]` as it compiles the model, initializes the VAD, and
opens the loopback device. **The first step is slow on the NPU** (see below) — it is
compiling, not hung.

To reproduce the benchmark:

```powershell
.\scripts\make_sample_wav.ps1                 # Japanese test clip via Windows TTS
.\scripts\prove_npu.ps1 -Device NPU           # RTF + proof the NPU ran it
.\scripts\load_test.ps1  -Device NPU          # under iGPU saturation
.\scripts\load_test.ps1  -Device GPU          # ... and compare
```

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
inference with counter-verified device attribution, WASAPI loopback capture, and the
full `live` pipeline.

Rough edges, honestly stated:

- Startup latency on the NPU (above) — the biggest usability problem.
- Utterance-level latency only. Text appears when an utterance ends; there are no
  streaming partials yet (LocalAgreement-2 is the intended fix).
- The benchmark clip is clean single-speaker TTS. Real meetings — overlapping
  speakers, noise, disfluencies — are harder, so treat the transcription quality here
  as optimistic. The RTF comparison between devices is unaffected.
- Occasional hallucination on trailing silence.
- Captures the whole system mix, so it picks up other applications' sound. Isolating
  one process needs `wasapi`'s `new_application_loopback_client`.
- Your own microphone is not captured — only what the speakers play.

## Layout

```
src/asr.rs                 OpenVINO WhisperPipeline wrapper (device switch + timing)
src/capture.rs             WASAPI loopback capture (Windows)
src/vad.rs                 Silero VAD streaming segmentation
src/audio.rs               downmix / resample helpers
src/bin/{live,bench,diag,capture_probe}.rs
scripts/                   setup + measurement (PowerShell) — see scripts/README.md
notes/                     benchmark results and environment facts
export_model.py            optional: convert a model yourself with optimum-intel
reference-python/          the Python prototype this was ported from, kept for
                           cross-checking against OpenVINO's Python API
```

## License

MIT — see [LICENSE](LICENSE).
