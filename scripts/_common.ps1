<#
  Shared helpers for the measurement scripts. Dot-source with:
      . (Join-Path $PSScriptRoot '_common.ps1')
#>

function Resolve-ProjectRoot {
  param([string]$Explicit = '')
  if ($Explicit) { return (Resolve-Path $Explicit).Path }
  return (Split-Path $PSScriptRoot -Parent)
}

<#
  Locate OpenVINO's setupvars.bat. Order: explicit arg, INTEL_OPENVINO_DIR
  (set by a previous setupvars run), then common install locations.
#>
function Resolve-SetupVars {
  param([string]$OpenVinoRoot = '')

  $candidates = @()
  if ($OpenVinoRoot) { $candidates += $OpenVinoRoot }
  if ($env:INTEL_OPENVINO_DIR) { $candidates += $env:INTEL_OPENVINO_DIR }
  $candidates += @('C:\ov-genai', 'C:\openvino', "${env:ProgramFiles(x86)}\Intel")

  foreach ($c in $candidates) {
    if (-not (Test-Path $c)) { continue }
    $direct = Join-Path $c 'setupvars.bat'
    if (Test-Path $direct) { return $direct }
    # Archives extract to a versioned subdirectory.
    $found = Get-ChildItem $c -Filter 'setupvars.bat' -Recurse -Depth 2 -ErrorAction SilentlyContinue |
             Select-Object -First 1
    if ($found) { return $found.FullName }
  }
  throw "setupvars.bat not found. Pass -OpenVinoRoot <path to the extracted OpenVINO GenAI archive>."
}

<#
  Identify the NPU by elimination.

  Windows exposes no NPU-specific performance counter set, but the NPU does show
  up in the "GPU Engine" counters as its own adapter LUID. Display adapters are
  registered under HKLM\SOFTWARE\Microsoft\DirectX; the NPU is not. So: take the
  LUIDs seen in the counters, drop the ones the DirectX registry knows about, and
  the remaining compute-only adapter is the NPU.

  Returns the LUID string (e.g. "0x00000000_0x00010964") or $null.
#>
function Get-NpuLuid {
  $samples = (Get-Counter -Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction Stop).CounterSamples

  $byLuid = @{}
  foreach ($s in $samples) {
    if ($s.InstanceName -match 'luid_(0x[0-9a-fA-F]+_0x[0-9a-fA-F]+).*engtype_(\w+)') {
      $l = $matches[1].ToLower(); $t = $matches[2].ToLower()
      if (-not $byLuid.ContainsKey($l)) { $byLuid[$l] = @{} }
      $byLuid[$l][$t] = $true
    }
  }

  # LUIDs of real display adapters, from the DirectX registry (stored as decimal).
  $display = @{}
  Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\DirectX' -ErrorAction SilentlyContinue | ForEach-Object {
    $p = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
    if ($null -ne $p.AdapterLuid) {
      $u = [uint64]$p.AdapterLuid
      $key = ("luid_0x{0:x8}_0x{1:x8}" -f (($u -shr 32) -band 0xFFFFFFFF), ($u -band 0xFFFFFFFF))
      $display[($key -replace '^luid_', '')] = $true
    }
  }

  foreach ($l in $byLuid.Keys) {
    if ($display.ContainsKey($l)) { continue }              # a display adapter
    $types = @($byLuid[$l].Keys)
    if ($types.Count -eq 1 -and $types[0] -eq 'compute') {   # compute-only => NPU
      return $l
    }
  }
  return $null
}

function Get-IgpuLuid {
  $display = @()
  Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\DirectX' -ErrorAction SilentlyContinue | ForEach-Object {
    $p = Get-ItemProperty $_.PSPath -ErrorAction SilentlyContinue
    if ($null -ne $p.AdapterLuid -and $p.Description -and $p.Description -notmatch 'Basic Render') {
      $u = [uint64]$p.AdapterLuid
      $display += ("0x{0:x8}_0x{1:x8}" -f (($u -shr 32) -band 0xFFFFFFFF), ($u -band 0xFFFFFFFF))
    }
  }
  if ($display.Count -gt 0) { return $display[0] }
  return $null
}

<# Sum of utilization across all engine instances of one adapter LUID. #>
function Get-AdapterUtil {
  param([Parameter(Mandatory)][string]$Luid,
        [Parameter(Mandatory)]$Samples)
  ($Samples | Where-Object { $_.InstanceName -like "*$Luid*" } |
     Measure-Object -Property CookedValue -Sum).Sum
}
