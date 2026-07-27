# Reference environment and Windows gotchas

Everything here was measured directly on the reference machine, not inferred.
Benchmark numbers live in [benchmark-results.md](benchmark-results.md).

## Reference machine

| | |
|---|---|
| Laptop | ThinkPad X13 Gen 5 |
| CPU | **Intel Core Ultra 7 155H** (Meteor Lake, 16 cores / 22 threads) |
| NPU | **Intel AI Boost**, driver **32.0.100.4724** (PCI `VEN_8086&DEV_7D1D`, class `ComputeAccelerator`) |
| iGPU | Intel Arc Graphics, driver 32.0.101.8724 (PCI `VEN_8086&DEV_7D55`) |
| RAM | 32 GB |
| OS | Windows 11, build 26200 |
| Toolchain | Rust 1.97.1 (MSVC) · VS Build Tools 2022 (VCTools) · OpenVINO GenAI **2026.2.1** |

OpenVINO documents **32.0.100.3104** as the minimum NPU driver; anything older can
fail to compile.

## Proving the NPU actually ran

**Windows exposes no NPU-specific performance counter set** — nothing under
`Get-Counter -ListSet *` matches NPU/Neural. But the NPU *does* appear in the
**GPU Engine** counters as its own adapter LUID, so it can be measured after all.

How to identify which LUID is the NPU, by elimination:

1. Collect the adapter LUIDs appearing in `\GPU Engine(*)\Utilization Percentage`.
2. Real **display** adapters are registered under `HKLM\SOFTWARE\Microsoft\DirectX`
   (with `AdapterLuid` as a decimal value). The NPU is not a display adapter, so it
   is absent there.
3. The leftover adapter that exposes **only a Compute engine** is the NPU.

On the reference machine that resolved to:

| LUID | In DirectX registry | Engines | Identity |
|---|---|---|---|
| `0x00000000_0x00010428` | `Intel(R) Arc(TM) Graphics` | 3D, VideoDecode, Copy, VideoProcessing, Compute, GSC | iGPU |
| `0x00000000_0x000108DB` | `Microsoft Basic Render Driver` | 3D only | software adapter |
| `0x00000000_0x00010964` | **absent** | **Compute only** (owned by pid 4 = System) | **NPU** |

`scripts/_common.ps1` implements this detection (`Get-NpuLuid`), so the scripts work
on other machines without hardcoding a LUID.

**Why bother:** selecting `device="NPU"` is not proof the NPU ran the work. Intel's
own Audacity plugin has a report of NPU 20% / CPU 80% while "NPU" was selected
([issue #364](https://github.com/intel/openvino-plugins-ai-audacity/issues/364)).
Without a counter check you can easily publish a CPU number as an "NPU" number.

Level Zero note: `ze_loader.dll` lives in `System32`, but the NPU's Level-Zero
driver (`ze_intel_npu64.dll` / `npu_driver_compiler.dll`) lives in the DriverStore
(`npu.inf_amd64_*`) and is resolved through the registry. Nothing extra to install.

## Windows gotchas hit while automating this

These cost real time; they apply to anyone driving a Windows box non-interactively
(CI, SSH, remote agents).

- **`winget` does not work in a non-interactive session.** It is on `PATH` at
  `%LOCALAPPDATA%\Microsoft\WindowsApps\winget.exe`, but running it — even via its
  real path under `C:\Program Files\WindowsApps\Microsoft.DesktopAppInstaller_*` —
  fails with *Access denied*. App-execution aliases need an interactive user
  context. **Install everything by direct download instead** (`curl.exe` ships with
  Windows and works fine).
- **The Microsoft Store Python alias is the same trap**: `python.exe` in
  `WindowsApps` is a 0-byte reparse stub and fails with the same error.
- **PowerShell 5.1 reads BOM-less `.ps1` as ANSI (cp932 on a Japanese system).**
  A script containing Japanese must be saved as **UTF-8 with BOM**, or every
  non-ASCII string turns into mojibake and the parse fails.
- **Here-strings (`@'...'@`) break with LF line endings.** If a script may be
  transferred from a Unix host, build multi-line strings from an array plus
  `[string]::Join` instead.
- **`cmd` truncates command lines at ~8191 characters**, so passing a long script
  via `powershell -EncodedCommand` silently fails. Copy the `.ps1` over and run it
  with `-File`.
- `scp` does not expand environment variables; use forward-slash absolute paths
  (`host:C:/Users/you/...`).

## No Python required

The Rust build needs no Python at all. Models are fetched straight from the
Hugging Face API with `curl.exe` (see `scripts/get_model.ps1`) — `huggingface-cli`
is not needed. `export_model.py` is only there if you want to convert a model
yourself with `optimum-intel`.

An OpenVINO GenAI `WhisperPipeline` model directory needs:

```
openvino_encoder_model.xml / .bin
openvino_decoder_model.xml / .bin
openvino_tokenizer.xml / .bin
openvino_detokenizer.xml / .bin
config.json  generation_config.json  preprocessor_config.json
tokenizer.json  tokenizer_config.json  vocab.json  merges.txt  ...
```

A directory that also contains `openvino_decoder_with_past_model.*` is the older
**stateless** export and will not run — see benchmark-results.md.

## Which OpenVINO archive to download

Download **one** file: the **GenAI** archive. It is a strict superset of the plain
runtime archive — it adds `openvino_genai.dll` / `openvino_genai_c.dll`,
`openvino_tokenizers.dll`, the matching `.lib` import libraries, and the GenAI
headers, and it already contains the NPU/GPU/CPU plugins.

```
https://storage.openvinotoolkit.org/repositories/openvino_genai/packages/<ver>/windows/openvino_genai_windows_<full-ver>_x86_64.zip
```

Two traps:

- The plain `openvino_toolkit_windows_*.zip` has **no GenAI content**, so the Rust
  bindings cannot link against it (`openvino-genai-sys` needs both
  `openvino_genai.lib` and `openvino_genai_c.lib`).
- The storage host **returns HTTP 200 with an HTML page for paths that do not
  exist**, so a 200 is not proof a URL is valid — check `Content-Type` /
  `Content-Length`, or consult
  [`filetree.json`](https://storage.openvinotoolkit.org/filetree.json), the
  machine-readable index of the whole bucket. The docs page for 2026.2.1 prints a
  directory/filename version mismatch that 404s this way.

The pip `openvino-genai` wheel is **not** a substitute: it ships only
`openvino_genai.dll` and the Python `.pyd`, with no `openvino_genai_c.dll` and no
import libraries.
