"""Live desktop-audio transcription: WASAPI loopback -> Silero VAD -> OpenVINO Whisper.

Captures whatever your default speakers are playing (a Google Meet call, a video,
etc.), segments it into utterances with VAD, and transcribes each on the chosen
device. Each line prints its own RTF so you can watch, live, whether the NPU is
keeping up.

  python live.py --model models/whisper-large-v3-turbo-int4-ov-npu --device NPU

Ctrl+C to stop.
"""
from __future__ import annotations

import argparse
import time

from mt.asr import WhisperASR
from mt.audio import open_loopback_stream, to_mono_16k
from mt.vad import SpeechSegmenter


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", required=True, help="OpenVINO IR model directory")
    ap.add_argument("--device", default="NPU", help="NPU | GPU | CPU")
    ap.add_argument("--language", default="ja")
    ap.add_argument("--max-segment", type=float, default=15.0)
    args = ap.parse_args()

    print(f"loading Whisper on {args.device} ...", flush=True)
    asr = WhisperASR(args.model, device=args.device, language=args.language)
    segmenter = SpeechSegmenter(max_segment_s=args.max_segment)

    p, stream, info = open_loopback_stream()
    print(f"capturing: {info['name']}  ({info['src_sr']} Hz, {info['channels']} ch)")
    print("listening... play audio on your speakers. Ctrl+C to stop.\n", flush=True)

    read_frames = int(info["src_sr"] * 0.1)  # ~100 ms reads
    try:
        while True:
            raw = stream.read(read_frames, exception_on_overflow=False)
            audio_16k = to_mono_16k(raw, info["channels"], info["src_sr"])
            for utterance in segmenter.feed(audio_16k):
                r = asr.transcribe(utterance)
                if not r.text:
                    continue
                stamp = time.strftime("%H:%M:%S")
                print(
                    f"[{stamp}] ({r.audio_seconds:4.1f}s RTF={r.rtf:.2f}) {r.text}",
                    flush=True,
                )
    except KeyboardInterrupt:
        print("\nstopping.")
    finally:
        stream.stop_stream()
        stream.close()
        p.terminate()


if __name__ == "__main__":
    main()
