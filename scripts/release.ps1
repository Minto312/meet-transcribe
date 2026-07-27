<#
.SYNOPSIS
  Build the signed NSIS installer and the updater manifest for a release.

.DESCRIPTION
  Runs `cargo tauri build` with OpenVINO on PATH, which produces three things in
  target\release\bundle\nsis\ :

      meet-transcribe_<version>_x64-setup.exe        the installer
      meet-transcribe_<version>_x64-setup.exe.sig    its minisign signature
      (this script then writes) latest.json          the updater manifest

  latest.json is what `plugins > updater > endpoints` in tauri.conf.json points
  at, so it must be uploaded as a release asset alongside the installer:

      gh release create v<version> --title v<version> --notes-file NOTES.md `
        target\release\bundle\nsis\meet-transcribe_<version>_x64-setup.exe `
        target\release\bundle\nsis\latest.json

  The `releases/latest/download/latest.json` URL always resolves to the newest
  non-prerelease, so no other step is needed to publish an update.

  SIGNING: the private key is expected at %USERPROFILE%\.tauri\meet-transcribe.key
  and to have no passphrase (`cargo tauri signer generate --ci`). `--ci` is passed
  to the build for the same reason: without it the bundler prompts for a
  passphrase on stdin, which hangs a non-interactive SSH session.

.EXAMPLE
  .\release.ps1
.EXAMPLE
  .\release.ps1 -Debug            # unsigned-ish smoke test of the bundler
#>
param(
  [string]$ProjectRoot = '',
  [string]$OpenVinoRoot = '',
  [string]$KeyPath = (Join-Path $env:USERPROFILE '.tauri\meet-transcribe.key'),
  # Build the bundle from a debug binary. Only useful to check the packaging
  # itself; the resulting installer is large and slow.
  [switch]$Debug,
  # Skip the build and just (re)write latest.json from what is already there.
  [switch]$ManifestOnly
)

$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '_common.ps1')

$proj = Resolve-ProjectRoot $ProjectRoot
$appDir = Join-Path $proj 'app'
$conf = Join-Path $appDir 'tauri.conf.json'

$config = Get-Content $conf -Raw -Encoding UTF8 | ConvertFrom-Json
$version = $config.version
$repo = 'Minto312/meet-transcribe'

# The version the updater compares against the feed comes from tauri.conf.json.
# A Cargo.toml that disagrees is confusing at best, so refuse to ship it.
foreach ($manifest in @((Join-Path $proj 'Cargo.toml'), (Join-Path $appDir 'Cargo.toml'))) {
  $line = Select-String -Path $manifest -Pattern '^version = "(.+)"' | Select-Object -First 1
  $found = $line.Matches[0].Groups[1].Value
  if ($found -ne $version) {
    throw "version mismatch: tauri.conf.json says $version but $manifest says $found"
  }
}

Write-Output ("project : " + $proj)
Write-Output ("version : " + $version)

$profileDir = if ($Debug) { 'debug' } else { 'release' }
$bundle = Join-Path $proj ("target\" + $profileDir + "\bundle\nsis")
$setupName = "meet-transcribe_${version}_x64-setup.exe"
$setup = Join-Path $bundle $setupName

if (-not $ManifestOnly) {
  if (-not (Test-Path $KeyPath)) {
    throw @"
signing key not found: $KeyPath
Generate one (once, and keep it -- updates cannot be signed without it):
    cargo tauri signer generate --ci -w $KeyPath
Then put the contents of $KeyPath.pub into
tauri.conf.json > plugins > updater > pubkey.
"@
  }

  $setupVars = Resolve-SetupVars $OpenVinoRoot
  Write-Output ("openvino: " + $setupVars)

  # TAURI_SIGNING_PRIVATE_KEY accepts either the key itself or a path to it.
  $env:TAURI_SIGNING_PRIVATE_KEY = $KeyPath
  # --ci makes the empty passphrase explicit instead of prompting for one.
  $flags = if ($Debug) { ' --debug' } else { '' }
  $build = 'call "' + $setupVars + '" >nul 2>&1 && cargo tauri build --ci' + $flags

  Push-Location $appDir
  & cmd /c $build
  $code = $LASTEXITCODE
  Pop-Location
  Remove-Item Env:\TAURI_SIGNING_PRIVATE_KEY
  if ($code -ne 0) { throw "cargo tauri build failed with exit code $code" }
}

if (-not (Test-Path $setup)) { throw "installer not found: $setup" }
$sigPath = "$setup.sig"
if (-not (Test-Path $sigPath)) {
  throw "signature not found: $sigPath (is bundle.createUpdaterArtifacts still true?)"
}

# The static manifest format the updater expects. `windows-x86_64` is the key it
# falls back to when no bundle-specific one (`windows-x86_64-nsis`) is present,
# so one entry covers both lookups.
$signature = (Get-Content $sigPath -Raw).Trim()
$url = "https://github.com/$repo/releases/download/v$version/$setupName"
$manifest = [ordered]@{
  version   = "v$version"
  pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
  platforms = [ordered]@{
    'windows-x86_64' = [ordered]@{
      signature = $signature
      url       = $url
    }
  }
}

$latest = Join-Path $bundle 'latest.json'
# ASCII-safe UTF-8 without a BOM: a BOM makes serde reject the JSON.
[System.IO.File]::WriteAllText(
  $latest,
  ($manifest | ConvertTo-Json -Depth 5),
  (New-Object System.Text.UTF8Encoding($false))
)

Write-Output ""
Write-Output "=== artifacts ==="
Get-Item $setup, $sigPath, $latest | ForEach-Object {
  Write-Output ("  " + $_.Name + "  " + [math]::Round($_.Length / 1MB, 2) + " MB")
}
Write-Output ""
Write-Output "=== next ==="
Write-Output ("  gh release create v$version --title v$version --notes-file <notes> \")
Write-Output ("    """ + $setup + """ \")
Write-Output ("    """ + $latest + """")
Write-Output "DONE"
