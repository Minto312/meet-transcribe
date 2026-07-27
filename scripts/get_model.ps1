<#
.SYNOPSIS
  Download an OpenVINO IR Whisper model from Hugging Face without Python.

.DESCRIPTION
  Lists the repo's files via the Hugging Face API and fetches each with curl.exe,
  so `huggingface-cli` (and therefore Python) is not needed on the target machine.

  Use the official OpenVINO conversions. Community "*-ov-npu" builds that ship
  openvino_decoder_with_past_model.* are the older stateless export and throw on
  OpenVINO GenAI 2026.x -- see notes/benchmark-results.md.

.EXAMPLE
  .\get_model.ps1 -Repo OpenVINO/whisper-medium-int8-ov
#>
param(
  [string]$Repo = 'OpenVINO/whisper-medium-int8-ov',
  [string]$Name = '',
  # Defaults to <repo root>/models (parent of this script's directory).
  [string]$ModelsDir = (Join-Path (Split-Path $PSScriptRoot -Parent) 'models')
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Continue'

if (-not $Name) { $Name = ($Repo -split '/')[-1] }
$dest = Join-Path $ModelsDir $Name
New-Item -ItemType Directory -Path $dest -Force | Out-Null

Write-Output "repo: $Repo"
Write-Output "dest: $dest"

$meta = Invoke-RestMethod -Uri "https://huggingface.co/api/models/$Repo" -TimeoutSec 60
$files = $meta.siblings | ForEach-Object { $_.rfilename } | Where-Object { $_ -ne '.gitattributes' }

$total = 0
foreach ($f in $files) {
  $out = Join-Path $dest $f
  $dir = Split-Path $out -Parent
  if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
  if (Test-Path $out) { $total += (Get-Item $out).Length; continue }
  & "$env:SystemRoot\System32\curl.exe" -sSL -o $out "https://huggingface.co/$Repo/resolve/main/$f"
  if ($LASTEXITCODE -ne 0) { Write-Output ("  FAILED " + $f); continue }
  $total += (Get-Item $out).Length
}

Write-Output ("total: " + [math]::Round($total / 1MB, 1) + " MB, files=" + $files.Count)
Get-ChildItem $dest -Filter '*.xml' | ForEach-Object { Write-Output ("  " + $_.Name) }
Write-Output "DONE"
