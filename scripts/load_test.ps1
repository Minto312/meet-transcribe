<#
.SYNOPSIS
  Measure a device while the iGPU is deliberately saturated.

.DESCRIPTION
  On a laptop the interesting question is not "which device is fastest when the
  machine is idle" but "which device still keeps up when the iGPU is already busy
  with desktop compositing and video playback". This saturates the iGPU with a
  continuous OpenVINO GPU workload, measures the target device under that
  contention, and lets you compare against its idle baseline from prove_npu.ps1.

  Reference result on a Core Ultra 7 155H (whisper-medium-int8, 37 s of audio):
    NPU  RTF 0.324 idle -> 0.285 contended   (no degradation)
    GPU  RTF 0.246 idle -> 0.476 contended   (1.9x worse)

.EXAMPLE
  .\load_test.ps1 -Device NPU
  .\load_test.ps1 -Device GPU
#>
param(
  [string]$Device    = 'NPU',
  [string]$Model     = 'models\whisper-medium-int8-ov',
  [string]$LoadModel = 'models\whisper-small-int8-ov',
  [string]$Audio     = 'sample_ja.wav',
  [int]$Runs         = 3,
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = ''
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Continue'
. (Join-Path $PSScriptRoot '_common.ps1')

$proj  = Resolve-ProjectRoot $ProjectRoot
$setup = Resolve-SetupVars  $OpenVinoRoot
$flag  = Join-Path $env:TEMP 'meet-transcribe-loadtest.flag'
$exe   = Join-Path $proj 'target\release\bench.exe'

$npu  = Get-NpuLuid
$igpu = Get-IgpuLuid
Remove-Item $flag -ErrorAction SilentlyContinue

# Background iGPU saturator. Load the model ONCE and keep inferring (--runs is
# large): relaunching the process in a loop would churn multi-GB model compiles,
# which is memory pressure rather than the sustained iGPU busy-ness that video
# playback actually produces.
$loadJob = Start-Job -ScriptBlock {
  param($setup, $proj, $exe, $loadModel, $audio)
  Set-Location $proj
  & cmd /c "call `"$setup`" >nul 2>&1 && `"$exe`" --model $loadModel --audio $audio --devices GPU --runs 200 >nul 2>&1"
} -ArgumentList $setup, $proj, $exe, $LoadModel, $Audio

$sampler = Start-Job -ScriptBlock {
  param($npu, $igpu, $flag)
  $maxNpu = 0.0; $maxIgpu = 0.0; $n = 0
  while (-not (Test-Path $flag)) {
    try {
      $s = (Get-Counter -Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction Stop).CounterSamples
      if ($npu)  { $a = ($s | Where-Object { $_.InstanceName -like "*$npu*" }  | Measure-Object -Property CookedValue -Sum).Sum; if ($a -gt $maxNpu)  { $maxNpu  = $a } }
      if ($igpu) { $b = ($s | Where-Object { $_.InstanceName -like "*$igpu*" } | Measure-Object -Property CookedValue -Sum).Sum; if ($b -gt $maxIgpu) { $maxIgpu = $b } }
      $n++
    } catch { }
    Start-Sleep -Milliseconds 250
  }
  [pscustomobject]@{ MaxNpu = $maxNpu; MaxIgpu = $maxIgpu; Samples = $n }
} -ArgumentList $npu, $igpu, $flag

Write-Output ("=== iGPU saturated in background; measuring " + $Device + " on " + $Model + " ===")
Start-Sleep -Seconds 20   # let the load actually spin up (it compiles its model first)

$cmd = 'call "' + $setup + '" >nul 2>&1 && "' + $exe + '" --model ' + $Model + ' --audio ' + $Audio + ' --devices ' + $Device + ' --runs ' + $Runs + ' 2>&1'
Push-Location $proj
$out = & cmd /c $cmd
Pop-Location
$out | ForEach-Object { Write-Output ("  [bench] " + $_) }

New-Item -ItemType File -Path $flag -Force | Out-Null
$r = Receive-Job -Job $sampler -Wait -AutoRemoveJob
Stop-Job $loadJob -ErrorAction SilentlyContinue
Remove-Job $loadJob -Force -ErrorAction SilentlyContinue
Remove-Item $flag -ErrorAction SilentlyContinue
Get-Process bench -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

Write-Output ""
Write-Output "=== counters during the contended run ==="
Write-Output ("  samples           : " + $r.Samples)
Write-Output ("  MAX NPU  util (%) : " + [math]::Round($r.MaxNpu, 2))
Write-Output ("  MAX iGPU util (%) : " + [math]::Round($r.MaxIgpu, 2))
