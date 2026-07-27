# scripts/ — setup and measurement helpers (PowerShell, Windows)

All of these run on the Windows target machine. Paths default to this repository,
and the OpenVINO installation is located automatically (explicit override:
`-OpenVinoRoot`).

| script | purpose |
|---|---|
| `release.ps1` | Build the **signed NSIS installer** and write the `latest.json` the updater reads. `-ManifestOnly` rewrites the manifest without rebuilding |
| `run_gui.ps1` | Launch the GUI with OpenVINO explicitly on `PATH`. Not normally needed — the app finds the runtime itself (see below) — but it is the override when the install is somewhere unusual. `-Build` compiles it first |
| `get_model.ps1` | Download an OpenVINO IR Whisper model straight from Hugging Face — **no Python / `huggingface-cli` needed**. `-Repo OpenVINO/whisper-medium-int8-ov`. The GUI's setup panel does the same thing from inside the app |
| `make_sample_wav.ps1` | Generate a 16 kHz mono Japanese test clip with the built-in Windows TTS voice |
| `prove_npu.ps1` | Run the benchmark **while sampling NPU/iGPU utilization**, so the result can be attributed to a device. `-Device NPU -Model models\whisper-medium-int8-ov` |
| `load_test.ps1` | Measure a device **while the iGPU is deliberately saturated** — the realistic condition on a laptop. `-Device NPU` / `-Device GPU` |
| `test_capture.ps1` | Play audio and confirm WASAPI loopback really captures the system mix |
| `test_engine.ps1` | Exercise the **GUI's engine** headlessly: compile a model, capture the played clip, stop, and confirm it drains back to idle. `-Device NPU` |
| `_common.ps1` | Shared helpers: locate `setupvars.bat`, auto-detect the NPU adapter |

## Typical order

```powershell
.\scripts\get_model.ps1 -Repo OpenVINO/whisper-medium-int8-ov
.\scripts\make_sample_wav.ps1
cargo build --release
.\scripts\prove_npu.ps1 -Device NPU
.\scripts\load_test.ps1  -Device NPU     # then -Device GPU, and compare
```

## When you need `run_gui.ps1`

Usually never: `meet-transcribe-app.exe` imports `openvino_genai_c.dll` with
`/DELAYLOAD`, so it starts before the DLL has to resolve, locates the OpenVINO
install and prepends the same directories to `PATH` that `setupvars.bat` would.

Reach for the script when the install is somewhere the search does not look, or
to pin a specific one with `-OpenVinoRoot`. (Version v0.1.0 of the GUI *did*
require it: the import was load-time, so the process died at load with
`0xC00004BC` and no message.)

## Releasing

`release.ps1` needs two things beyond the normal build: the **Tauri CLI**
(`cargo install tauri-cli --locked --version "^2"`) and the **updater signing
key** at `%USERPROFILE%\.tauri\meet-transcribe.key`, generated once with

```powershell
cargo tauri signer generate --ci -w $env:USERPROFILE\.tauri\meet-transcribe.key
```

`--ci` gives the key no passphrase, and `release.ps1` passes `--ci` to the build
for the matching reason: otherwise the bundler prompts for a passphrase on stdin
and a non-interactive SSH session hangs. The script also refuses to build when
`tauri.conf.json` and the two `Cargo.toml` files disagree about the version,
because the version in `tauri.conf.json` is the one the updater compares against
the feed.

Upload both `meet-transcribe_<ver>_x64-setup.exe` and `latest.json` to the GitHub
release; see [Releasing](../README.md#releasing).

## Why "prove" the NPU

`device="NPU"` succeeding is **not** evidence the NPU executed the work; OpenVINO
can fall back silently (Intel's Audacity plugin has a report of NPU 20% / CPU 80%
while NPU was selected). `prove_npu.ps1` samples

```
\GPU Engine(*luid_<npu>*engtype_compute)\Utilization Percentage
```

for the duration of the run and prints the peak. Windows has no NPU-specific
counter set, so the NPU's adapter LUID is found by elimination against the DirectX
registry — implemented in `_common.ps1::Get-NpuLuid`, and explained in
[../notes/environment.md](../notes/environment.md).

## Running these over SSH

They work fine non-interactively, with caveats that cost real time:

- **Copy the `.ps1` over and run it with `-File`.** `powershell -EncodedCommand`
  hits `cmd`'s ~8191-character command-line limit for anything non-trivial.
- **Save scripts containing Japanese as UTF-8 *with BOM*.** PowerShell 5.1 reads
  BOM-less `.ps1` as ANSI and will mojibake/fail to parse.
- **`winget` is unusable non-interactively** (Access denied, even via its real
  path). Install by direct download; `curl.exe` ships with Windows.

```bash
scp scripts/prove_npu.ps1 <host>:C:/Users/<you>/prove_npu.ps1
ssh <host> "powershell -NoProfile -ExecutionPolicy Bypass -File C:/Users/<you>/prove_npu.ps1"
```
