<#
.SYNOPSIS
  Run the benchmark and prove which accelerator actually executed.

.DESCRIPTION
  Selecting device="NPU" is not by itself evidence that the NPU ran the work --
  OpenVINO can silently fall back (Intel's Audacity plugin issue #364 reports
  NPU 20% / CPU 80% while "NPU" was selected). This samples NPU and iGPU
  utilization for the duration of the run and reports the peak, so an RTF number
  can be attributed to a device with confidence.

  The NPU LUID is auto-detected (see _common.ps1); it differs per machine.

.EXAMPLE
  .\prove_npu.ps1 -Device NPU -Model models\whisper-medium-int8-ov
#>
param(
  [string]$Device = 'NPU',
  [string]$Model  = 'models\whisper-medium-int8-ov',
  [string]$Audio  = 'sample_ja.wav',
  [int]$Runs      = 2,
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = ''
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot '_common.ps1')

$proj  = Resolve-ProjectRoot $ProjectRoot
$setup = Resolve-SetupVars  $OpenVinoRoot
$flag  = Join-Path $env:TEMP 'meet-transcribe-bench.flag'

$npu  = Get-NpuLuid
$igpu = Get-IgpuLuid
if (-not $npu) { Write-Output "WARNING: no NPU adapter auto-detected; NPU evidence unavailable." }
Write-Output ("project : " + $proj)
Write-Output ("openvino: " + $setup)
Write-Output ("NPU LUID: " + $(if ($npu) { $npu } else { '(none)' }) + "   iGPU LUID: " + $(if ($igpu) { $igpu } else { '(none)' }))
Write-Output ("=== running bench on " + $Device + " (" + $Model + ") ===")

Remove-Item $flag -ErrorAction SilentlyContinue

$sampler = Start-Job -ScriptBlock {
  param($npu, $igpu, $flag)
  # Query only this adapter's instances. Pulling '\GPU Engine(*)' costs ~1 s per
  # call on a machine with hundreds of engine instances, which is slow enough to
  # miss the whole burst on a short run and report a false "NPU never ran".
  $paths = @()
  if ($npu)  { $paths += "\GPU Engine(*luid_$npu*)\Utilization Percentage" }
  if ($igpu) { $paths += "\GPU Engine(*luid_$igpu*)\Utilization Percentage" }
  $maxNpu = 0.0; $maxIgpu = 0.0; $n = 0
  $deadline = (Get-Date).AddMinutes(20)
  while ((Get-Date) -lt $deadline) {
    try {
      $s = (Get-Counter -Counter $paths -ErrorAction Stop).CounterSamples
      if ($npu)  { $a = ($s | Where-Object { $_.Path -like "*$npu*" }  | Measure-Object -Property CookedValue -Sum).Sum; if ($a -gt $maxNpu)  { $maxNpu  = $a } }
      if ($igpu) { $b = ($s | Where-Object { $_.Path -like "*$igpu*" } | Measure-Object -Property CookedValue -Sum).Sum; if ($b -gt $maxIgpu) { $maxIgpu = $b } }
      $n++
    } catch { }
    if (Test-Path $flag) { break }
    Start-Sleep -Milliseconds 100
  }
  [pscustomobject]@{ MaxNpu = $maxNpu; MaxIgpu = $maxIgpu; Samples = $n }
} -ArgumentList $npu, $igpu, $flag

Start-Sleep -Seconds 1

$exe = Join-Path $proj 'target\release\bench.exe'
$cmd = 'call "' + $setup + '" >nul 2>&1 && "' + $exe + '" --model ' + $Model + ' --audio ' + $Audio + ' --devices ' + $Device + ' --runs ' + $Runs + ' 2>&1'
Push-Location $proj
$out = & cmd /c $cmd
Pop-Location
$out | ForEach-Object { Write-Output ("  [bench] " + $_) }

New-Item -ItemType File -Path $flag -Force | Out-Null
$r = Receive-Job -Job $sampler -Wait -AutoRemoveJob
Remove-Item $flag -ErrorAction SilentlyContinue

Write-Output ""
Write-Output "=== counter evidence ==="
Write-Output ("  samples taken     : " + $r.Samples)
Write-Output ("  MAX NPU  util (%) : " + [math]::Round($r.MaxNpu, 2))
Write-Output ("  MAX iGPU util (%) : " + [math]::Round($r.MaxIgpu, 2))
Write-Output ""
# Utilization is summed across engine instances, so it can exceed 100%.
if ($r.MaxNpu -gt 1.0) {
  Write-Output "  VERDICT: NPU utilization rose above idle -> the NPU really executed."
} elseif ($r.Samples -lt 8) {
  # A zero backed by only a handful of samples is not evidence of absence: a short
  # run (small model, few iterations) can finish between polls.
  Write-Output "  VERDICT: INCONCLUSIVE -- only $($r.Samples) samples for a run this short."
  Write-Output "           Re-run with more iterations (-Runs 5) or a larger model."
} else {
  Write-Output "  VERDICT: NPU stayed ~0 -> the work did NOT run on the NPU."
}
