$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\lib\workspace.ps1"
ParseZedWorkspace
Set-Location $env:ZED_WORKSPACE

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$installPath = $null
if (Test-Path $vswhere) {
    $installPath = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath
}
if (-not $installPath) {
    $installPath = 'C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools'
}
$launch = Join-Path $installPath 'Common7\Tools\Launch-VsDevShell.ps1'
Push-Location
& $launch -Arch amd64 -HostArch amd64
Pop-Location
Set-Location $env:ZED_WORKSPACE

$target = 'x86_64-pc-windows-msvc'
$CargoOutDir = "./target/$target/release-fast"
$env:CARGO_BUILD_JOBS = '1'
$env:CARGO_INCREMENTAL = '0'
$env:ZED_RELEASE_CHANNEL = (Get-Content 'crates\zed\RELEASE_CHANNEL' -Raw).Trim()
$env:RELEASE_CHANNEL = $env:ZED_RELEASE_CHANNEL

# Keep rustc stable on Windows:
# - one Cargo job (no multi-crate parallelism)
# - no incremental / no LTO / no debug info
# - opt-level 2 + many codegen units (smaller LLVM modules)
$cargoArgs = @(
    'build', '-j', '1',
    '--profile', 'release-fast',
    '--config', 'profile.release-fast.opt-level=2',
    '--config', 'profile.release-fast.debug=0',
    '--config', 'profile.release-fast.codegen-units=32',
    '--config', 'profile.release-fast.lto=false',
    '--config', 'profile.release-fast.incremental=false',
    '--target', $target
)

Write-Host "Building lead (release-fast, opt-level=2, debug=0, codegen-units=32, lto=false, incremental=false)..."
Write-Host "Tip: close browsers/IDEs you do not need; peak rustc+link memory is still high near the end."

& cargo @cargoArgs --package cli --package auto_update_helper
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

& cargo @cargoArgs --package LEAD --bin lead
if ($LASTEXITCODE -ne 0) {
    Write-Host "Lead cargo build failed with exit $LASTEXITCODE"
    exit $LASTEXITCODE
}

$lead = Get-Item "$CargoOutDir\lead.exe"
Write-Host "Lead build OK: $($lead.FullName) ($([math]::Round($lead.Length/1MB,1)) MB) $($lead.LastWriteTime)"

$releaseDir = "./target/$target/release"
New-Item -ItemType Directory -Force -Path $releaseDir | Out-Null
Copy-Item "$CargoOutDir\lead.exe" "$releaseDir\lead.exe" -Force
Copy-Item "$CargoOutDir\cli.exe" "$releaseDir\cli.exe" -Force
Copy-Item "$CargoOutDir\auto_update_helper.exe" "$releaseDir\auto_update_helper.exe" -Force

Write-Host "Packaging installer with fresh binary..."
& "$PSScriptRoot\bundle-lead-windows.ps1" -SkipBuild -Install
exit $LASTEXITCODE
