# Dot-source to use an installed SDK or the repository-local tools.
param([string]$GStreamerRoot)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../../..')).Path
$localCargo = Join-Path $repo '.tools/cargo/bin'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $localCargo 'cargo.exe'))) {
    $env:CARGO_HOME = Join-Path $repo '.tools/cargo'
    $env:RUSTUP_HOME = Join-Path $repo '.tools/rustup'
    $env:PATH = "$localCargo;$env:PATH"
}
$localGcc = Join-Path $repo '.tools/w64devkit/bin'
# Configure MSVC even from an ordinary PowerShell (and restricted environments
# where rustc cannot discover Visual Studio through the registry).
$vswhere = "${env:ProgramFiles(x86)}/Microsoft Visual Studio/Installer/vswhere.exe"
if (-not (Get-Command link.exe -ErrorAction SilentlyContinue) -and (Test-Path -LiteralPath $vswhere)) {
    $vs = & $vswhere -latest -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($vs) {
        $vc = Get-ChildItem -LiteralPath "$vs/VC/Tools/MSVC" -Directory | Sort-Object Name -Descending | Select-Object -First 1
        $sdkBase = "${env:ProgramFiles(x86)}/Windows Kits/10"
        $sdk = Get-ChildItem -LiteralPath "$sdkBase/Lib" -Directory | Sort-Object Name -Descending | Select-Object -First 1
        if ($vc -and $sdk) {
            $env:PATH = "$($vc.FullName)/bin/Hostx64/x64;$sdkBase/bin/$($sdk.Name)/x64;$env:PATH"
            $env:LIB = "$($vc.FullName)/lib/x64;$($sdk.FullName)/ucrt/x64;$($sdk.FullName)/um/x64"
            $env:INCLUDE = "$($vc.FullName)/include;$sdkBase/Include/$($sdk.Name)/ucrt;$sdkBase/Include/$($sdk.Name)/shared;$sdkBase/Include/$($sdk.Name)/um"
        }
    }
}
if ((Get-Command rustc -ErrorAction SilentlyContinue) -and ((& rustc -vV) -match 'host:.*windows-gnu') -and (Test-Path -LiteralPath $localGcc)) { $env:PATH = "$localGcc;$env:PATH" }
if (-not $GStreamerRoot) {
    $candidates = @($env:GSTREAMER_1_0_ROOT_MSVC_X86_64, $env:GSTREAMER_1_0_ROOT_MINGW_X86_64,
        (Join-Path $repo '.tools/gstreamer'), "$env:LOCALAPPDATA/Programs/gstreamer/1.0/msvc_x86_64",
        "$env:ProgramFiles/gstreamer/1.0/msvc_x86_64", 'C:/gstreamer/1.0/msvc_x86_64')
    $GStreamerRoot = $candidates | Where-Object { $_ -and (Test-Path (Join-Path $_ 'bin/gst-inspect-1.0.exe')) } | Select-Object -First 1
}
if (-not $GStreamerRoot) { throw 'GStreamer was not found. Install the x64 development package; see client/windows/README.md.' }
$GStreamerRoot = (Resolve-Path -LiteralPath $GStreamerRoot).Path
$env:PATH = "$(Join-Path $GStreamerRoot 'bin');$env:PATH"
$env:PKG_CONFIG_PATH = Join-Path $GStreamerRoot 'lib/pkgconfig'
foreach ($name in @('pkg-config.exe','pkgconf.exe')) {
    $candidate = Join-Path $GStreamerRoot "bin/$name"
    if (Test-Path -LiteralPath $candidate) { $env:PKG_CONFIG = $candidate; break }
}
