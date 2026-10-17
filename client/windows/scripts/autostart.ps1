param([switch]$Disable)
$ErrorActionPreference = 'Stop'
$key = 'HKCU:/Software/Microsoft/Windows/CurrentVersion/Run'
if ($Disable) {
    Remove-ItemProperty -LiteralPath $key -Name 'AV Relay' -ErrorAction SilentlyContinue
    Write-Output 'AV Relay will no longer start at login.'
} else {
    $launcher = (Resolve-Path "$PSScriptRoot/start.ps1").Path
    $value = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$launcher`" -AtLogin"
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -LiteralPath $key -Name 'AV Relay' -Value $value -PropertyType String -Force | Out-Null
    Write-Output 'AV Relay will start at login. Keep this checkout and GStreamer in their current locations.'
}
