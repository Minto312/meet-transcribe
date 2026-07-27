<#
.SYNOPSIS
  Generate a Japanese 16 kHz mono test WAV with the built-in Windows TTS voice.

.DESCRIPTION
  Produces a ~37 s meeting-style Japanese clip using "Microsoft Haruka" (ja-JP),
  so the benchmark has deterministic input without shipping an audio file.

  NOTE: this is clean single-speaker synthetic speech and is therefore easier
  than a real meeting (overlapping speakers, noise, disfluencies). It is fine for
  comparing RTF across devices; do not read the transcription accuracy as
  representative of real calls.

.EXAMPLE
  .\make_sample_wav.ps1
#>
param([string]$OutFile = '')

$ProgressPreference='SilentlyContinue'
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Speech

$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
Write-Output "=== installed voices ==="
foreach ($v in $s.GetInstalledVoices()) {
  $i = $v.VoiceInfo
  Write-Output ("  " + $i.Name + " | culture=" + $i.Culture + " | gender=" + $i.Gender)
}

$ja = $s.GetInstalledVoices() | Where-Object { $_.VoiceInfo.Culture.Name -like 'ja*' } | Select-Object -First 1
if ($ja) {
  $s.SelectVoice($ja.VoiceInfo.Name)
  Write-Output ("selected JA voice: " + $ja.VoiceInfo.Name)
} else {
  Write-Output "!! no Japanese voice installed; default voice will be used"
}

# Built as an array (no here-string: those break when the file has LF endings).
$lines = @(
  '本日の定例ミーティングを始めます。まず先週の進捗から共有します。',
  '音声認識の処理をエヌピーユーで動かす検証を行いました。',
  'グラフィックスは画面描画と動画再生で使われているため、推論をエヌピーユーに逃がす方針で進めています。',
  '次に、リアルタイム性の評価について報告します。',
  '実時間に対する処理時間の比率が一を下回れば実用的と判断できます。',
  '最後に、来週までの課題を確認します。質問があればお願いします。'
)
$text = [string]::Join(' ', $lines)

if (-not $OutFile) { $OutFile = Join-Path (Split-Path $PSScriptRoot -Parent) 'sample_ja.wav' }
$path = $OutFile
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
$s.SetOutputToWaveFile($path, $fmt)
$s.Rate = 0
$s.Speak($text)
$s.SetOutputToNull()
$s.Dispose()

$f = Get-Item $path
$secs = ($f.Length - 44) / (16000 * 2)
Write-Output ""
Write-Output ("wrote: " + $path)
Write-Output ("size: " + $f.Length + " bytes")
Write-Output ("duration_sec: " + [math]::Round($secs,1))
Write-Output "=== DONE ==="
