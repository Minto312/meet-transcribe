<#
.SYNOPSIS
  Verify WASAPI loopback capture end-to-end, isolated from the ASR half.

.DESCRIPTION
  Starts capture_probe, plays the sample WAV through the default output device,
  and reports the captured signal level. A non-zero peak/rms proves the system
  mix is really being captured (no virtual audio cable required).

.EXAMPLE
  .\test_capture.ps1
#>
param(
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = '',
  [string]$Audio = ''
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot '_common.ps1')

$proj  = Resolve-ProjectRoot $ProjectRoot
$setup = Resolve-SetupVars  $OpenVinoRoot
$log   = Join-Path $env:TEMP 'meet-transcribe-capture.txt'
if (-not $Audio) { $Audio = Join-Path $proj 'sample_ja.wav' }
Remove-Item $log -ErrorAction SilentlyContinue

Write-Output "=== build capture_probe ==="
Push-Location $proj
& cmd /c "call `"$setup`" >nul 2>&1 && set PATH=%USERPROFILE%\.cargo\bin;%PATH% && cargo build --release --bin capture_probe 2>&1" | Select-Object -Last 5
Pop-Location

Write-Output ""
Write-Output "=== start capture_probe, then play audio into the system mix ==="
$exe = Join-Path $proj 'target\release\capture_probe.exe'
$cmd = 'call "' + $setup + '" >nul 2>&1 && "' + $exe + '" > "' + $log + '" 2>&1'
$p = Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', $cmd -WorkingDirectory $proj -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 3

try {
  Add-Type -AssemblyName presentationCore
  $player = New-Object System.Windows.Media.MediaPlayer
  $player.Open([uri]$Audio)
  Start-Sleep -Seconds 1
  $player.Volume = 1.0
  $player.Play()
  Write-Output ("  playing " + $Audio + " ...")
  Start-Sleep -Seconds 18
  $player.Stop(); $player.Close()
} catch { Write-Output ("  playback FAILED: " + $_.Exception.Message) }

Start-Sleep -Seconds 6
Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Get-Process capture_probe -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

Write-Output ""
Write-Output "=== capture_probe output ==="
if (Test-Path $log) { Get-Content $log | ForEach-Object { Write-Output ("  " + $_) } } else { Write-Output "  (no log)" }
