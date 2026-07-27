<#
.SYNOPSIS
  Build (optionally) and launch the meet-transcribe GUI with OpenVINO on PATH.

.DESCRIPTION
  The GUI statically imports openvino_genai_c.dll, so the OpenVINO DLLs must
  resolve when Windows loads the process -- before any of the app's own code
  runs, which means the app cannot repair its own PATH. Launching the exe
  directly exits immediately with 0xC00004BC and no message. This script sources
  setupvars.bat first and then launches the window detached.

  It works both inside the repository and from a release zip: the exe is looked
  for next to this script first, then under target\release.

  The GUI needs no Node toolchain: the front end is static files embedded at
  compile time, so plain `cargo build` is enough.

.EXAMPLE
  .\run_gui.ps1            # release zip, or an already-built checkout
.EXAMPLE
  .\run_gui.ps1 -Build     # in the repository, compile first
#>
param(
  [switch]$Build,
  [switch]$Debug,
  [string]$Exe = '',
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = ''
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '_common.ps1')

$setup      = Resolve-SetupVars $OpenVinoRoot
$profileDir = if ($Debug) { 'debug' } else { 'release' }

# A release zip puts the exe beside this script; a checkout puts it under
# target\<profile>\. Prefer whichever exists, unless -Build says otherwise.
$sideBySide = Join-Path $PSScriptRoot 'meet-transcribe-app.exe'
$proj = ''
if (-not $Exe) {
  if ((Test-Path $sideBySide) -and -not $Build) {
    $Exe = $sideBySide
  } else {
    $proj = Resolve-ProjectRoot $ProjectRoot
    $Exe  = Join-Path $proj ("target\" + $profileDir + "\meet-transcribe-app.exe")
  }
}

Write-Output ("openvino: " + $setup)
if ($proj) { Write-Output ("project : " + $proj) }

if ($Build -or -not (Test-Path $Exe)) {
  if (-not $proj) { $proj = Resolve-ProjectRoot $ProjectRoot }
  if (-not (Test-Path (Join-Path $proj 'Cargo.toml'))) {
    throw "not found: $Exe (and there is no Cargo project at $proj to build it from)"
  }
  Write-Output ("=== building meet-transcribe-app (" + $profileDir + ") ===")
  $flags = if ($Debug) { '' } else { ' --release' }
  $build = 'call "' + $setup + '" >nul 2>&1 && cargo build -p meet-transcribe-app' + $flags
  Push-Location $proj
  & cmd /c $build
  $code = $LASTEXITCODE
  Pop-Location
  if ($code -ne 0) { throw "cargo build failed with exit code $code" }
}

if (-not (Test-Path $Exe)) { throw "not found after build: $Exe" }

# `start ""` hands the exe off to a new process and returns, so the shell (or an
# SSH session) is not held open for the lifetime of the window.
Write-Output ("=== launching " + $Exe + " ===")
$run = 'call "' + $setup + '" >nul 2>&1 && start "" "' + $Exe + '"'
& cmd /c $run

Write-Output "launched. Transcripts are saved under %APPDATA%\com.minto312.meet-transcribe\sessions."
