"""Export a HuggingFace Whisper model to OpenVINO IR for NPU/GPU/CPU.

Thin wrapper around `optimum-cli export openvino`. Run this in the EXPORT
environment (requirements-export.txt: optimum-intel + nncf + a pinned
transformers), which is separate from the lean inference environment.

INT4/INT8 weights are recommended for the NPU (smaller, faster, fits better).

  python export_model.py --model openai/whisper-large-v3-turbo \
      --out models/whisper-large-v3-turbo-int4-ov --weight-format int4

NOTE: You can skip export entirely by downloading a pre-converted NPU model, e.g.
  huggingface-cli download FluidInference/whisper-large-v3-turbo-int4-ov-npu \
      --local-dir models/whisper-large-v3-turbo-int4-ov-npu
"""
from __future__ import annotations

import argparse
import subprocess
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", default="openai/whisper-large-v3-turbo")
    ap.add_argument("--out", required=True)
    ap.add_argument(
        "--weight-format", default="int4", choices=["fp16", "int8", "int4"]
    )
    ap.add_argument(
        "--legacy-npu",
        action="store_true",
        help="Add --disable-stateful for OpenVINO < 2025.1 (older NPU export path).",
    )
    args = ap.parse_args()

    cmd = [
        "optimum-cli", "export", "openvino",
        "--model", args.model,
        "--weight-format", args.weight_format,
    ]
    if args.legacy_npu:
        cmd += ["--task", "automatic-speech-recognition-with-past", "--disable-stateful"]
    cmd.append(args.out)

    print(" ".join(cmd), flush=True)
    return subprocess.call(cmd)


if __name__ == "__main__":
    sys.exit(main())
