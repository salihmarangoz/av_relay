param([switch]$Minimized, [switch]$AtLogin, [switch]$Doctor, [switch]$Debug, [string]$GStreamerRoot)
. "$PSScriptRoot/env.ps1" -GStreamerRoot $GStreamerRoot
$buildProfile = if ($Debug) { 'debug' } else { 'release' }
$exe = Join-Path $PSScriptRoot "../target/$buildProfile/av-relay-windows.exe"
if (-not (Test-Path -LiteralPath $exe)) { throw 'Build the receiver first: client/windows/scripts/build.ps1' }
$arguments = @()
if ($Minimized) { $arguments += '--minimized' }
if ($AtLogin) { $arguments += '--at-login' }
if ($Doctor) {
    $arguments += '--doctor'
    $report = Join-Path $env:LOCALAPPDATA 'AV Relay/diagnostics.txt'
    if (Test-Path -LiteralPath $report) { Remove-Item -LiteralPath $report }
    $process = Start-Process -FilePath $exe -ArgumentList $arguments -WindowStyle Hidden -Wait -PassThru
    if (Test-Path -LiteralPath $report) { Get-Content -LiteralPath $report }
    else { throw 'Diagnostics failed before writing a report. Check GStreamer DLL paths and av-relay.log.' }
    if ($process.ExitCode -ne 0) { throw 'Diagnostics failed; see AV Relay/av-relay.log under LOCALAPPDATA.' }
} else {
    if ($Minimized) { Start-Process -FilePath $exe -ArgumentList $arguments -WindowStyle Hidden }
    elseif ($arguments.Count) { Start-Process -FilePath $exe -ArgumentList $arguments -WindowStyle Normal }
    else { Start-Process -FilePath $exe -WindowStyle Normal }
}
