param([string]$GStreamerRoot)
$ErrorActionPreference = 'Continue'
try { . "$PSScriptRoot/env.ps1" -GStreamerRoot $GStreamerRoot }
catch { Write-Output $_.Exception.Message }
foreach ($name in @('cargo','rustc')) {
    if (Get-Command $name -ErrorAction SilentlyContinue) { & $name --version }
    else { Write-Output "MISSING: $name (https://rust-lang.org/tools/install/)" }
}
if (Get-Command gst-inspect-1.0 -ErrorAction SilentlyContinue) {
    & gst-inspect-1.0 --version
    foreach ($plugin in @('webrtcbin','nicesrc','nicesink','dtlssrtpdec','avdec_h264','opusdec','videoconvertscale','appsink')) {
        & gst-inspect-1.0 --exists $plugin
        if ($LASTEXITCODE -ne 0) { Write-Output "MISSING GStreamer plugin: $plugin" }
    }
}
$camera = 'Registry::HKEY_CLASSES_ROOT/CLSID/{A3FCE0F5-3493-419F-958A-ABA1250EC20B}/InprocServer32'
if (Test-Path -LiteralPath $camera) { Write-Output 'OBS Virtual Camera: registered' }
else { Write-Output 'MISSING: OBS Virtual Camera (https://obsproject.com/download)' }
Write-Output 'Microphone: install VB-CABLE if CABLE Input and CABLE Output are absent in Windows Sound settings: https://vb-audio.com/Cable/'
