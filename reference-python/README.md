# reference-python/ — the Python prototype this was ported from

**Not the deliverable.** The Rust implementation in the repository root is the real
thing; it is what was built, run, and benchmarked. This directory is kept only
because OpenVINO GenAI's Python API has far more examples in the wild, which makes
it handy for cross-checking when something in the Rust path misbehaves.

It is **not maintained** and has **not** been re-run since the port. In particular it
predates two findings that the Rust version bakes in:

- Use the official `OpenVINO/whisper-*-int8-ov` models. Community `*-ov-npu` builds
  that ship `openvino_decoder_with_past_model.*` are the older **stateless** export
  and throw on OpenVINO GenAI 2026.x — on every device.
- **`medium` is the largest model that runs on the NPU**; `large-v3-turbo` throws
  there (it does work on the iGPU).

For measured results see the top-level [README](../README.md) and
[notes/benchmark-results.md](../notes/benchmark-results.md). **No benchmark numbers
are quoted in this file** — the real ones were taken with the Rust binaries.

## Shape of it

Same pipeline as the Rust version:

```
PyAudioWPatch (WASAPI loopback) -> downmix + resample to 16 kHz -> Silero VAD
  -> openvino_genai.WhisperPipeline(device = NPU | GPU | CPU)
```

| file | |
|---|---|
| `bench.py` | per-device RTF benchmark |
| `live.py` | live loopback transcription |
| `mt/asr.py` | WhisperPipeline wrapper with timing |
| `mt/audio.py` | WASAPI loopback capture + 16 kHz mono conversion |
| `mt/vad.py` | Silero VAD segmentation |

```powershell
py -3.12 -m venv .venv; .\.venv\Scripts\Activate.ps1
pip install -r requirements-live.txt
python bench.py --model ..\models\whisper-medium-int8-ov --audio ..\sample_ja.wav --devices NPU CPU
```

Unlike the Rust build, this route needs Python. The Rust binaries do not.

## Known Python-side pitfalls

| symptom | cause / fix |
|---|---|
| Whisper breaks during `optimum-cli export openvino` | pin `transformers==4.51.3` (4.52 also works). **4.53 is broken.** |
| no loopback device found | check which output device is default (Bluetooth headset?) |
| nothing appears while silent | expected — WASAPI loopback delivers no buffers during true silence |
| `NPU` not available | install Intel AI Boost driver 32.0.100.3104+ |
