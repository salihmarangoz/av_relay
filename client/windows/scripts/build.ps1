param([switch]$Debug, [string]$GStreamerRoot)
. "$PSScriptRoot/env.ps1" -GStreamerRoot $GStreamerRoot
$client = (Resolve-Path "$PSScriptRoot/..").Path
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Install Rust first: https://rust-lang.org/tools/install/' }
$cargoArgs = @('build','--locked','--manifest-path', (Join-Path $client 'Cargo.toml'))
if (-not $Debug) { $cargoArgs += '--release' }
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { throw 'Rust build failed.' }
if (-not $Debug) {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut((Join-Path $client 'AV Relay.lnk'))
    $shortcut.TargetPath = "$env:SystemRoot/System32/WindowsPowerShell/v1.0/powershell.exe"
    $shortcut.Arguments = '-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "' + (Join-Path $PSScriptRoot 'start.ps1') + '"'
    $shortcut.WorkingDirectory = $client
    $shortcut.IconLocation = (Join-Path $client 'target/release/av-relay-windows.exe') + ',0'
    $shortcut.Save()
    Write-Output "Ready: $(Join-Path $client 'AV Relay.lnk')"
}
