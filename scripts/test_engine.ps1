<#
.SYNOPSIS
  Verify the GUI's engine end-to-end without a window.

.DESCRIPTION
  The GUI's hard part is the threading, not the pixels: a model compile that
  takes minutes, a capture thread that must stop within a second, and a queue
  that has to drain before a run counts as finished. engine_probe drives exactly
  that state machine from a console, so it can be checked over SSH on a machine
  with no interactive desktop.

  This waits for the model to compile, plays the sample clip into the system
  mix, then lets the probe stop and drain. Exit code 0 means the engine went
  Idle -> LoadingModel -> Running -> Finalizing -> Idle cleanly.

  Note: the probe's own stdout is UTF-8. Reading the log with PowerShell 5.1's
  default encoding mojibakes the Japanese, hence the -Encoding UTF8 below.

.EXAMPLE
  .\test_engine.ps1 -Device NPU -Model models\whisper-small-int8-ov
#>
param(
  [string]$Device = 'NPU',
  [string]$Model  = 'models\whisper-small-int8-ov',
  [int]$Seconds   = 30,
  [switch]$Mic,
  [string]$Audio  = '',
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = ''
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot '_common.ps1')

$proj  = Resolve-ProjectRoot $ProjectRoot
$setup = Resolve-SetupVars  $OpenVinoRoot
$log   = Join-Path $env:TEMP 'meet-transcribe-engine.txt'
if (-not $Audio) { $Audio = Join-Path $proj 'sample_ja.wav' }
Remove-Item $log -ErrorAction SilentlyContinue

Write-Output "=== build engine_probe ==="
Push-Location $proj
& cmd /c ('call "' + $setup + '" >nul 2>&1 && cargo build --release --bin engine_probe 2>&1') |
  Select-Object -Last 3
Pop-Location

$exe = Join-Path $proj 'target\release\engine_probe.exe'
$args = '--model ' + $Model + ' --device ' + $Device + ' --seconds ' + $Seconds
if ($Mic) { $args += ' --mic' }
$cmd = 'call "' + $setup + '" >nul 2>&1 && "' + $exe + '" ' + $args + ' > "' + $log + '" 2>&1'

Write-Output ""
Write-Output ("=== start engine_probe (" + $Device + ", " + $Model + ") ===")
$p = Start-Process -FilePath 'cmd.exe' -ArgumentList '/c', $cmd -WorkingDirectory $proj -PassThru -WindowStyle Hidden

# NPU compilation is minutes; only start playing once capture is actually live,
# otherwise the clip is over before anything is listening.
Write-Output "waiting for the model to compile ..."
$ready = $false
for ($i = 0; $i -lt 210; $i++) {
  Start-Sleep -Seconds 2
  if ((Test-Path $log) -and (Select-String -Path $log -Pattern 'model ready' -Quiet)) { $ready = $true; break }
  if ($p.HasExited) { break }
}
Write-Output ("model ready: " + $ready)

$player = $null
if ($ready -and -not $Mic) {
  try {
    Add-Type -AssemblyName presentationCore
    $player = New-Object System.Windows.Media.MediaPlayer
    $player.Open([uri]$Audio)
    Start-Sleep -Seconds 1
    $player.Volume = 1.0
    $player.Play()
    Write-Output ("playing " + $Audio + " into the system mix ...")
  } catch { Write-Output ("playback FAILED: " + $_.Exception.Message) }
}

$p.WaitForExit(300000) | Out-Null
if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue; Write-Output "TIMED OUT" }
if ($player) { try { $player.Stop(); $player.Close() } catch { } }

Write-Output ""
Write-Output "=== engine_probe output ==="
if (Test-Path $log) { Get-Content $log -Encoding UTF8 } else { Write-Output "(no log)" }
Write-Output ("EXIT=" + $p.ExitCode)
