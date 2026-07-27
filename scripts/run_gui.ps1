<#
.SYNOPSIS
  Build (optionally) and launch the meet-transcribe GUI with OpenVINO on PATH.

.DESCRIPTION
  The GUI links against openvino_genai_c.dll, so the OpenVINO DLLs must resolve
  when the process loads -- far too early for the app to fix its own PATH. That
  is why the exe cannot simply be double-clicked: run it through this script,
  which sources setupvars.bat first and then launches the window detached.

  The GUI needs no Node toolchain: the front end is static files embedded at
  compile time, so plain `cargo build` is enough.

.EXAMPLE
  .\run_gui.ps1 -Build
#>
param(
  [switch]$Build,
  [switch]$Debug,
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = ''
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '_common.ps1')

$proj    = Resolve-ProjectRoot $ProjectRoot
$setup   = Resolve-SetupVars  $OpenVinoRoot
$profile = if ($Debug) { 'debug' } else { 'release' }
$exe     = Join-Path $proj ("target\" + $profile + "\meet-transcribe-app.exe")

Write-Output ("project : " + $proj)
Write-Output ("openvino: " + $setup)

if ($Build -or -not (Test-Path $exe)) {
  Write-Output ("=== building meet-transcribe-app (" + $profile + ") ===")
  $flags = if ($Debug) { '' } else { ' --release' }
  $build = 'call "' + $setup + '" >nul 2>&1 && cargo build -p meet-transcribe-app' + $flags
  Push-Location $proj
  & cmd /c $build
  $code = $LASTEXITCODE
  Pop-Location
  if ($code -ne 0) { throw "cargo build failed with exit code $code" }
}

if (-not (Test-Path $exe)) { throw "not found after build: $exe" }

# `start ""` hands the exe off to a new process and returns, so the shell (or an
# SSH session) is not held open for the lifetime of the window.
Write-Output ("=== launching " + $exe + " ===")
$run = 'call "' + $setup + '" >nul 2>&1 && start "" "' + $exe + '"'
Push-Location $proj
& cmd /c $run
Pop-Location

Write-Output "launched. Transcripts are saved under %APPDATA%\com.minto312.meet-transcribe\sessions."
