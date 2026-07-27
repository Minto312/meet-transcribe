"""Streaming speech segmentation via Silero VAD.

Whisper is a 30-second batch model, not a true streaming model. The pragmatic
approach for a live prototype is: use VAD to detect when an utterance ends
(a short silence), then transcribe that whole utterance at once. This gives
"finalize at end of each utterance" latency, which is fine for a benchmark and
for reading a meeting transcript. (A lower-latency refinement -- LocalAgreement-2
partial hypotheses -- is a documented next step; see README.)
"""
from __future__ import annotations

import numpy as np

WINDOW = 512  # Silero VADIterator requires exactly 512 samples per call @ 16 kHz


class SpeechSegmenter:
    def __init__(
        self,
        threshold: float = 0.5,
        min_silence_ms: int = 500,
        speech_pad_ms: int = 200,
        max_segment_s: float = 15.0,
    ) -> None:
        from silero_vad import VADIterator, load_silero_vad

        # VADIterator internally casts inputs to torch tensors, so torch is
        # required regardless of the onnx flag. (A truly torch-free path is to
        # drive the raw Silero ONNX model via onnxruntime with a hand-written
        # state machine, or to swap in webrtcvad -- see README "次のステップ".)
        self._model = load_silero_vad()
        self._vad = VADIterator(
            self._model,
            sampling_rate=16000,
            threshold=threshold,
            min_silence_duration_ms=min_silence_ms,
            speech_pad_ms=speech_pad_ms,
        )
        self._buf = np.empty(0, dtype=np.float32)  # leftover samples < WINDOW
        self._speech: list[np.ndarray] = []        # windows of the active utterance
        self._in_speech = False
        self._max_samples = int(max_segment_s * 16000)

    def feed(self, audio_16k: np.ndarray):
        """Feed arbitrary-length 16 kHz mono float32; yield completed utterances
        (np.float32 arrays) as they end or reach the max segment length."""
        self._buf = np.concatenate([self._buf, audio_16k.astype(np.float32)])
        while len(self._buf) >= WINDOW:
            win = self._buf[:WINDOW]
            self._buf = self._buf[WINDOW:]

            event = self._vad(win)
            if event and "start" in event:
                self._in_speech = True
                self._speech = []
            if self._in_speech:
                self._speech.append(win)
            if event and "end" in event and self._in_speech:
                self._in_speech = False
                if self._speech:
                    yield np.concatenate(self._speech)
                self._speech = []
            elif self._in_speech and self._collected() >= self._max_samples:
                # Whisper caps at 30s; force-flush over-long utterances.
                yield np.concatenate(self._speech)
                self._speech = []

    def _collected(self) -> int:
        return sum(len(s) for s in self._speech)

    def reset(self) -> None:
        self._vad.reset_states()
        self._buf = np.empty(0, dtype=np.float32)
        self._speech = []
        self._in_speech = False
