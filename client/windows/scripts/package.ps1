# Builds a per-user installer with the runtime dependency closure, no SDKs.
param([string]$GStreamerRoot, [string]$Iscc, [switch]$SkipBuild)
$ErrorActionPreference='Stop'
. "$PSScriptRoot/env.ps1" -GStreamerRoot $GStreamerRoot
$client=(Resolve-Path "$PSScriptRoot/..").Path
$repo=(Resolve-Path "$client/../..").Path
if (-not $SkipBuild) { & "$PSScriptRoot/build.ps1" -GStreamerRoot $GStreamerRoot }
$gst=Split-Path (Split-Path $env:PKG_CONFIG)
$version=[regex]::Match((Get-Content "$client/Cargo.toml" -Raw),'(?m)^version = "([^"]+)"').Groups[1].Value
if (-not $version) { throw 'Package version missing' }
if (-not $Iscc) {
    $Iscc=@("$repo/.tools/inno/ISCC.exe","${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe","$env:ProgramFiles/Inno Setup 7/ISCC.exe",(Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source) | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
}
if (-not $Iscc) { throw 'Install Inno Setup 6.7+ or pass -Iscc C:\path\ISCC.exe' }
$dist=Join-Path $client 'dist'
$stage=Join-Path $dist "stage-$version"
if (Test-Path -LiteralPath $stage) {
    $resolved=(Resolve-Path -LiteralPath $stage).Path
    if (-not $resolved.StartsWith($dist+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) { throw 'Stage path escapes dist' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
foreach ($dir in @('bin','lib/gstreamer-1.0','libexec/gstreamer-1.0','share')) { New-Item -ItemType Directory -Path (Join-Path $stage $dir) -Force | Out-Null }
Copy-Item -LiteralPath "$client/target/release/av-relay-windows.exe" -Destination "$stage/bin"
# Keep just the plugins used by the shared receiver pipeline.
$plugins=@('app','audioconvert','audioresample','coreelements','dtls','libav','nice','opus','playback','rtp','rtpmanager','sctp','srtp','typefindfunctions','videoconvertscale','videoparsersbad','webrtc')
foreach ($plugin in $plugins) { Copy-Item -LiteralPath "$gst/lib/gstreamer-1.0/gst$plugin.dll" -Destination "$stage/lib/gstreamer-1.0" }
Copy-Item -LiteralPath "$gst/libexec/gstreamer-1.0/gst-plugin-scanner.exe" -Destination "$stage/libexec/gstreamer-1.0"
# Resolve PE imports (including delay imports) transitively. Only DLLs from
# GStreamer and the MSVC redistributable directory are copied; OS DLLs stay OS-owned.
$vswhere="${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
$vs=& $vswhere -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$crt=Get-ChildItem "$vs/VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT" -Directory | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $crt) { throw 'MSVC x64 redistributable DLLs not found' }
foreach ($dll in Get-ChildItem -LiteralPath $crt.FullName -Filter '*.dll') { Copy-Item -LiteralPath $dll.FullName -Destination "$stage/bin" }
$queue=[Collections.Generic.Queue[string]]::new()
Get-ChildItem -LiteralPath $stage -Recurse -File | Where-Object { $_.Extension -in '.dll','.exe' } | ForEach-Object { $queue.Enqueue($_.FullName) }
$seen=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
while ($queue.Count -gt 0) {
    $file=$queue.Dequeue()
    if (-not $seen.Add($file)) { continue }
    $imports=& dumpbin /nologo /dependents $file
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect imports: $file" }
    foreach ($line in $imports) {
        if ($line -match '^\s+([\w.+-]+\.dll)\s*$') {
            $name=$Matches[1]; $source=Join-Path $gst "bin/$name"; $dest=Join-Path $stage "bin/$name"
            if ((Test-Path -LiteralPath $source) -and -not (Test-Path -LiteralPath $dest)) { Copy-Item -LiteralPath $source -Destination $dest; $queue.Enqueue($dest) }
            elseif (-not (Test-Path -LiteralPath $dest) -and -not (Test-Path -LiteralPath "$env:SystemRoot/System32/$name") -and $name -notlike 'api-ms-*' -and $name -notlike 'ext-ms-*') { throw "Missing runtime dependency $name (from $file)" }
        }
    }
}
Copy-Item -LiteralPath "$gst/share/licenses" -Destination "$stage/share" -Recurse
Copy-Item -LiteralPath "$gst/share/versions.txt" -Destination "$stage/share"
Copy-Item -LiteralPath "$repo/LICENSE","$client/README.md","$client/packaging/THIRD-PARTY.md" -Destination $stage
# Smoke-test the staged package without the developer GStreamer PATH/plugins.
$savedPath=$env:PATH
$savedPlugins=$env:GST_PLUGIN_SYSTEM_PATH_1_0
$savedScanner=$env:GST_PLUGIN_SCANNER_1_0
try {
    $env:PATH="$stage/bin;$env:SystemRoot/System32;$env:SystemRoot"
    $env:GST_PLUGIN_SYSTEM_PATH_1_0="$stage/lib/gstreamer-1.0"
    $env:GST_PLUGIN_SCANNER_1_0="$stage/libexec/gstreamer-1.0/gst-plugin-scanner.exe"
    $env:GST_REGISTRY=Join-Path $dist 'package-test-registry.bin'
    $p=Start-Process -FilePath "$stage/bin/av-relay-windows.exe" -ArgumentList '--doctor' -Wait -PassThru -WindowStyle Hidden
    if ($p.ExitCode -ne 0) { throw 'Staged runtime smoke test failed; inspect diagnostics.txt' }
} finally { $env:PATH=$savedPath; $env:GST_PLUGIN_SYSTEM_PATH_1_0=$savedPlugins; $env:GST_PLUGIN_SCANNER_1_0=$savedScanner }
& $Iscc /Qp "/DAppVersion=$version" "/DStageDir=$stage" "$client/packaging/installer.iss"
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
$setup=Join-Path $dist "AV-Relay-$version-Windows-x64-Setup.exe"
$hash=(Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText("$setup.sha256","$hash  $([IO.Path]::GetFileName($setup))`n")
Write-Output "Installer: $setup"
