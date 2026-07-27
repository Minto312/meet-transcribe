"""Benchmark Whisper RTF across OpenVINO devices on THIS machine.

This is the decisive tool: there is no published RTF number for Whisper on the
Core Ultra 7 155H NPU, so you have to measure it yourself. Run it once idle,
then AGAIN while playing a YouTube video (to reproduce your real desktop load),
and compare how each device holds up.

  python bench.py --model models/whisper-large-v3-turbo-int4-ov-npu \
      --audio sample_ja.wav --devices NPU CPU GPU --runs 5

RTF < 1  => faster than real time (can keep up with a live call)
RTF < ~0.5 => comfortable headroom for streaming
"""
from __future__ import annotations

import argparse
import statistics

import numpy as np
import soundfile as sf
import soxr

from mt.asr import WhisperASR


def load_audio(path: str) -> np.ndarray:
    audio, sr = sf.read(path, dtype="float32", always_2d=False)
    if audio.ndim > 1:
        audio = audio.mean(axis=1)
    if sr != 16000:
        audio = soxr.resample(audio, sr, 16000)
    return np.ascontiguousarray(audio, dtype=np.float32)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", required=True, help="OpenVINO IR model directory")
    ap.add_argument("--audio", required=True, help="WAV file (any sr, mono/stereo)")
    ap.add_argument("--devices", nargs="+", default=["NPU", "CPU", "GPU"])
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--language", default="ja")
    args = ap.parse_args()

    audio = load_audio(args.audio)
    dur = len(audio) / 16000.0
    print(f"audio: {args.audio}  ({dur:.1f}s @ 16k mono)\n")

    summary = []
    for device in args.devices:
        print(f"=== {device} ===")
        try:
            asr = WhisperASR(args.model, device=device, language=args.language)
        except Exception as exc:  # noqa: BLE001 -- report and move to next device
            print(f"  load/compile FAILED: {exc}\n")
            summary.append((device, None))
            continue

        # First run pays model compile + kernel JIT; report it, but don't count it.
        warm = asr.transcribe(audio)
        print(f"  warmup: {warm.infer_seconds:6.2f}s  RTF={warm.rtf:.3f}")

        rtfs = []
        for i in range(args.runs):
            r = asr.transcribe(audio)
            rtfs.append(r.rtf)
            print(f"  run {i + 1}: {r.infer_seconds:6.2f}s  RTF={r.rtf:.3f}")

        med = statistics.median(rtfs)
        verdict = "OK (keeps up live)" if med < 1 else "TOO SLOW for live"
        print(f"  --> median RTF={med:.3f}  [{verdict}]")
        print(f"  text: {warm.text[:120]}\n")
        summary.append((device, med))

    print("=== summary (median RTF, lower is better) ===")
    for device, med in summary:
        if med is None:
            print(f"  {device:>4}: failed to load")
        else:
            print(f"  {device:>4}: {med:.3f}  {'OK' if med < 1 else 'too slow'}")


if __name__ == "__main__":
    main()
