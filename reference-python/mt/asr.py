"""OpenVINO GenAI Whisper wrapper with device switching (NPU/GPU/CPU) and timing.

The whole point of this prototype is to measure whether the Core Ultra 7 155H's
NPU can keep up with live audio (RTF < 1) while leaving the iGPU free for the
desktop compositor and video playback. So every transcription is timed and the
device is a single constructor argument you can flip.
"""
from __future__ import annotations

import time
from dataclasses import dataclass

import numpy as np
import openvino_genai as ov_genai

SAMPLE_RATE = 16000


@dataclass
class TranscribeResult:
    text: str
    audio_seconds: float
    infer_seconds: float

    @property
    def rtf(self) -> float:
        """Real-Time Factor = compute time / audio duration. < 1 keeps up live."""
        if self.audio_seconds <= 0:
            return float("nan")
        return self.infer_seconds / self.audio_seconds


class WhisperASR:
    def __init__(
        self,
        model_dir: str,
        device: str = "CPU",
        language: str = "ja",
        task: str = "transcribe",
    ) -> None:
        self.device = device
        # On recent OpenVINO, device="NPU" auto-routes to the static pipeline
        # (fixed shapes + pre-allocated KV cache). If you hit a shape/compile
        # error on NPU with an older OpenVINO, pass STATIC_PIPELINE=True here.
        self.pipe = ov_genai.WhisperPipeline(model_dir, device)

        cfg = self.pipe.get_generation_config()
        # Whisper language tokens look like "<|ja|>". Force Japanese so the model
        # doesn't waste a decode step auto-detecting the language every segment.
        cfg.language = f"<|{language}|>"
        cfg.task = task
        self.config = cfg

    def transcribe(self, audio_16k_mono: np.ndarray) -> TranscribeResult:
        audio = np.ascontiguousarray(audio_16k_mono, dtype=np.float32)
        seconds = len(audio) / SAMPLE_RATE
        t0 = time.perf_counter()
        result = self.pipe.generate(audio, self.config)
        dt = time.perf_counter() - t0

        texts = getattr(result, "texts", None)
        text = (texts[0] if texts else str(result)).strip()
        return TranscribeResult(text=text, audio_seconds=seconds, infer_seconds=dt)
