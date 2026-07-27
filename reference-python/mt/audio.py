"""WASAPI loopback capture (Windows) + resampling to 16 kHz mono float32.

WASAPI loopback records whatever the default output device is playing -- no
Stereo Mix, no VB-CABLE virtual cable needed. This captures the *whole* system
mix (remote Meet participants + any other app sound). Your own microphone is a
separate stream and is intentionally NOT captured here.
"""
from __future__ import annotations

import numpy as np

TARGET_SR = 16000


def to_mono_16k(frames_int16: bytes, channels: int, src_sr: int) -> np.ndarray:
    """Interleaved int16 bytes -> float32 mono @ 16 kHz in [-1, 1]."""
    import soxr

    audio = np.frombuffer(frames_int16, dtype=np.int16).astype(np.float32) / 32768.0
    if channels > 1:
        audio = audio.reshape(-1, channels).mean(axis=1)
    if src_sr != TARGET_SR:
        audio = soxr.resample(audio, src_sr, TARGET_SR)
    return np.ascontiguousarray(audio, dtype=np.float32)


def open_loopback_stream(frames_per_buffer: int = 4800):
    """Open a WASAPI loopback input stream on the default speaker.

    Returns (pyaudio_instance, stream, info) where info has channels/src_sr/name.
    Remember to stream.stop_stream(); stream.close(); pyaudio_instance.terminate().
    """
    import pyaudiowpatch as pyaudio

    p = pyaudio.PyAudio()
    try:
        wasapi = p.get_host_api_info_by_type(pyaudio.paWASAPI)
    except OSError as exc:  # WASAPI not available
        p.terminate()
        raise RuntimeError("WASAPI host API not available on this system.") from exc

    default_out = p.get_device_info_by_index(wasapi["defaultOutputDevice"])

    # The default output device isn't itself an input; find its loopback twin.
    if not default_out.get("isLoopbackDevice", False):
        loopback = None
        for candidate in p.get_loopback_device_info_generator():
            if default_out["name"] in candidate["name"]:
                loopback = candidate
                break
        if loopback is None:
            p.terminate()
            raise RuntimeError(
                f"No WASAPI loopback device found for default speaker "
                f"'{default_out['name']}'."
            )
        default_out = loopback

    channels = int(default_out["maxInputChannels"])
    src_sr = int(default_out["defaultSampleRate"])
    stream = p.open(
        format=pyaudio.paInt16,
        channels=channels,
        rate=src_sr,
        frames_per_buffer=frames_per_buffer,
        input=True,
        input_device_index=default_out["index"],
    )
    info = {"channels": channels, "src_sr": src_sr, "name": default_out["name"]}
    return p, stream, info
