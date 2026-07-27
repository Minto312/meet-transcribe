# Benchmark results — Whisper on a Core Ultra 7 155H NPU

Measured 2026-07-27 on real hardware. Environment details: [environment.md](environment.md).

- OpenVINO **GenAI 2026.2.1** (archive install) · NPU driver **32.0.100.4724**
- Rust 1.97.1 · MSVC 14.44 · `openvino-genai` crate **0.11**
- Test audio: **37.0 s**, 16 kHz mono Japanese, generated with the Windows TTS voice
  "Microsoft Haruka" (ja-JP), scripted as meeting speech
- **RTF** = compute time ÷ audio duration. **< 1 keeps up with live audio.**

> ⚠️ The clip is **clean single-speaker synthetic speech**, easier than a real Google
> Meet call (overlapping speakers, noise, disfluencies). Transcription quality below
> is therefore optimistic. The RTF *comparison between devices* is unaffected, which
> is what these measurements are for.

---

## 1. Headline: the NPU is immune to iGPU contention

The decisive result. Same work, measured with the iGPU deliberately saturated
(whisper-medium-int8):

| Device | RTF idle | **RTF under iGPU saturation** | Degradation |
|---|---|---|---|
| **NPU** | 0.324 | **0.285** | **none** (within noise; marginally better) |
| GPU (Arc iGPU) | 0.246 | **0.476** | **1.9× worse** |

The load was continuous Whisper inference on the GPU (`whisper-small-int8`, model
loaded **once** and inferred in a loop — closer to sustained video playback than
relaunching a process, which would just churn multi-GB compiles). iGPU utilization
held around 120% (summed across engine instances) throughout.

→ **On a laptop whose iGPU is already busy with desktop compositing and video, the
NPU is the right target.** The iGPU's small idle lead disappears under the exact
conditions you were trying to design around.

## 2. Per-device (whisper-medium-int8, idle)

| Device | RTF (median) | NPU util | iGPU util | Verdict |
|---|---|---|---|---|
| **NPU** | **0.20 – 0.32** (varies run to run) | 96% | — | **usable** |
| GPU | 0.246 | 0% | 119% | fast, but occupies the iGPU |
| CPU | **0.998** | 0% | — | **not viable** — no headroom at all |

CPU RTF ≈ 1.0 means 37 s of audio takes 37 s to process. Nothing left for live use.

## 3. Per model size (NPU)

| Model | RTF | NPU util | Load / compile | Japanese quality |
|---|---|---|---|---|
| tiny-int8 | 0.027 | 44% | ~2 s | poor |
| base-int8 | 0.047 | 80% | ~20 s | mediocre |
| small-int8 | 0.108 | 96% | **~60 s** | good |
| **medium-int8** | **0.20–0.32** | 96% | **~170 s** | **excellent** (≈ turbo) |
| large-v3-turbo-int8 | **fails** | 0% | — | — |

**`medium` is the ceiling on this NPU.** `large-v3-turbo` constructs successfully but
`generate()` throws `unknown exception` — consistent with
[openvino.genai #1965](https://github.com/openvinotoolkit/openvino.genai/issues/1965)
(turbo failing/hanging on NPU).

If you need turbo, it runs on the **iGPU only** (RTF 0.246 — but it occupies the iGPU).

## 4. ★ Model-format trap: use the official conversions

**`FluidInference/whisper-large-v3-turbo-int4-ov-npu` does not work.**

- Symptom: `WhisperPipeline::new` succeeds, then **`generate()` throws
  `unknown exception` on every device** (CPU, GPU and NPU alike).
- Bisected with `diag`: it fails even on 5 s of audio with default settings, so it is
  not audio length and not the language config.
- Cause: it ships **`openvino_decoder_with_past_model.*`** — the older **stateless**
  export. The official `OpenVINO/*` models are **stateful** (no `decoder_with_past`)
  and work fine.
- Fix: **use `OpenVINO/whisper-*-int8-ov`.**

Worth noting because desk research pointed at the FluidInference INT4 build as *the*
NPU model; on real hardware it was the one thing that never ran.

## 5. ★ NPU compilation is not cached

Same model, two consecutive process launches:

| Model | 1st load | 2nd load | Cached? |
|---|---|---|---|
| small-int8 | 62.7 s | 56.4 s | **no meaningful gain** |
| medium-int8 | 164.6 s | 177.4 s | **none** |

The NPU driver's UMD cache is supposed to be on by default, but it is not helping
here — matching
[openvino.genai #1992 "CACHE_DIR not being used with NPU"](https://github.com/openvinotoolkit/openvino.genai/issues/1992).
And the Rust API offers no workaround: `WhisperPipeline::new(path, device)` takes no
properties, so `ov::cache_dir` cannot be set at all.

**Design consequence:** every process start pays it (small ≈ 60 s, medium ≈ 170 s).
**Build a long-running process** — compile once, then stream. For a meeting
transcriber you launch once, that is acceptable. If you need fast startup, use
base/tiny (seconds).

> This is also why `live` appears to hang at `[1/3] loading Whisper …`. It is
> compiling. `bench` prints `load/compile: N s` separately so the two costs are
> distinguishable.

## 6. Language must be a token

- `set_language("<|ja|>")` → **works**
- `set_language("ja")` → **throws**

`src/asr.rs` uses the token form.

## 7. End-to-end (live)

Verified by playing the sample WAV through the machine's own speakers and letting
`live` capture it back via WASAPI loopback:

```
[1/3] loading Whisper on NPU ...
[2/3] Whisper ready. initializing Silero VAD ...
[3/3] VAD ready. opening WASAPI loopback ...
capturing: <default output device> (16000 Hz, 2 ch)
( 2.8s RTF=0.10) 本日の定例ミーティングを始めます
```

Capture verified in isolation with `capture_probe`:

```
total: 1662 chunks, 265889 samples (16.6s), peak=0.9424, rms=0.14628
VERDICT: real audio captured from the system mix.
```

- WASAPI `autoconvert` performs the 48 kHz → 16 kHz conversion in the driver, so no
  resampling is needed on the live path.
- No virtual audio cable required.

### Hallucination on near-silence (fixed)

Initially, one playback produced the classic Whisper filler
「ご視聴ありがとうございました」 about five times: the VAD was committing very short,
nearly-silent segments and handing them to Whisper.

**Fix:** a **0.7 s minimum utterance length** in `src/vad.rs`. After it, the same
run yielded the seven real utterances in order, with a single trailing hallucination
after playback stopped:

```
( 2.8s RTF=0.10) 本日の定例ミーティングを始めます
( 2.2s RTF=0.06) まず先週の進捗から共有します
( 3.3s RTF=0.06) 音声認識の処理をNPUで動かす検証を行いました
( 5.8s RTF=0.04) グラフィックスは画面描画と…NPUに逃がす方針で進めています
( 3.2s RTF=0.05) 次に、リアルタイム性の評価について報告します
( 4.3s RTF=0.05) 実時間に対する処理時間の比率が…実用的と判断できます
( 2.7s RTF=0.05) 最後に来週までの課題を確認します
```

(Text accuracy is low here because this run used **tiny**; medium is far better — see
§3. Transcripts above are cleaned of console encoding artifacts.)

Remaining: one hallucination still appears on trailing silence. Tuning the VAD
threshold, or a `no_speech_threshold`-style post-filter, would address it.

## 8. Recommended configuration

| Goal | Model | Device | Why |
|---|---|---|---|
| **Default** | **medium-int8** | **NPU** | RTF 0.2–0.3, excellent accuracy, leaves the iGPU alone. The ~3 min startup amortizes in a resident process |
| Fast startup | small-int8 | NPU | RTF 0.108, good accuracy, ~60 s startup |
| Maximum accuracy | large-v3-turbo-int8 | **GPU only** | Not supported on the NPU; occupies the iGPU and degrades 1.9× under load |
| Avoid | any | CPU | RTF ≈ 1.0, no headroom |

## 9. Reproducing

```powershell
# RTF plus load/compile time
target\release\bench.exe --model models\whisper-medium-int8-ov --audio sample_ja.wav --devices NPU --runs 3

# ... with proof of which device actually executed
.\scripts\prove_npu.ps1 -Device NPU -Model models\whisper-medium-int8-ov

# ... under a saturated iGPU
.\scripts\load_test.ps1 -Device NPU -Model models\whisper-medium-int8-ov
```

Device attribution comes from
`\GPU Engine(*luid_<npu>*engtype_compute)\Utilization Percentage`; the NPU's adapter
LUID is detected automatically (`scripts/_common.ps1`), with the reasoning in
[environment.md](environment.md).
