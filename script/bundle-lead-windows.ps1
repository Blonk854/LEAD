[CmdletBinding()]
Param(
    [Parameter()][Alias('i')][switch]$Install,
    [Parameter()][Alias('h')][switch]$Help,
    [Parameter()][Alias('a')][string]$Architecture,
    [Parameter()][switch]$SkipBuild
)

. "$PSScriptRoot/lib/workspace.ps1"

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true

$buildSuccess = $false

$OSArchitecture = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    "X64" { "x86_64" }
    "Arm64" { "aarch64" }
    default { throw "Unsupported architecture" }
}

$Architecture = if ($Architecture) {
    $Architecture
} else {
    $OSArchitecture
}

$CargoOutDir = "./target/$Architecture-pc-windows-msvc/release"
$target = "$Architecture-pc-windows-msvc"

function Get-VSArch {
    param([string]$Arch)
    switch ($Arch) {
        "x86_64" { "amd64" }
        "aarch64" { "arm64" }
    }
}

function Initialize-VsDevShell {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $installPath = $null
    if (Test-Path $vswhere) {
        $installPath = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath
    }
    if (-not $installPath) {
        $candidates = @(
            "C:\Program Files\Microsoft Visual Studio\2022\Community",
            "C:\Program Files\Microsoft Visual Studio\2022\BuildTools",
            "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools",
            "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
        )
        foreach ($candidate in $candidates) {
            if (Test-Path (Join-Path $candidate "Common7\Tools\Launch-VsDevShell.ps1")) {
                $installPath = $candidate
                break
            }
        }
    }
    if (-not $installPath) {
        throw "Visual Studio Build Tools not found. Install VS 2022/2026 Build Tools with the C++ workload."
    }

    $launchScript = Join-Path $installPath "Common7\Tools\Launch-VsDevShell.ps1"
    Write-Host "Using Visual Studio at: $installPath"
    Push-Location
    & $launchScript -Arch (Get-VSArch -Arch $Architecture) -HostArch (Get-VSArch -Arch $OSArchitecture)
    Pop-Location
}

function Get-InnoSetupPath {
    $candidates = @(
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe"
    )
    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            return $candidate
        }
    }
    throw @"
Inno Setup 6 is required to build the LEAD installer.
Install it with: winget install -e --id JRSoftware.InnoSetup
"@
}

if ($Help) {
    Write-Output @"
Usage: bundle-lead-windows.ps1 [-Install] [-Help] [-Architecture x86_64|aarch64]

Build a LEAD Windows installer that installs side-by-side with Zed:
  - Install dir: %LOCALAPPDATA%\Programs\LEAD
  - User data:   %APPDATA%\LEAD
  - Binary:      LEAD.exe (CLI: bin\LEAD.exe)

Options:
  -Architecture, -a  Target architecture (default: host)
  -Install, -i       Run the installer after building
  -SkipBuild         Package existing release binaries (skip cargo)
  -Help, -h          Show this help
"@
    exit 0
}

Initialize-VsDevShell

Push-Location -Path crates/zed
$channel = Get-Content "RELEASE_CHANNEL"
$env:ZED_RELEASE_CHANNEL = $channel
$env:RELEASE_CHANNEL = $channel
Pop-Location

function PrepareForBundle {
    if (Test-Path "$innoDir") {
        Remove-Item -Path "$innoDir" -Recurse -Force
    }
    New-Item -Path "$innoDir" -ItemType Directory -Force
    Copy-Item -Path "$env:ZED_WORKSPACE\crates\zed\resources\windows\*" -Destination "$innoDir" -Recurse -Force
    $leadSh = "$env:ZED_WORKSPACE\crates\zed\resources\windows\lead.sh"
    if (Test-Path $leadSh) {
        Copy-Item -Path $leadSh -Destination "$innoDir\lead.sh" -Force
    }
    New-Item -Path "$innoDir\bin" -ItemType Directory -Force
    New-Item -Path "$innoDir\tools" -ItemType Directory -Force
    rustup target add $target
}

function GenerateLicenses {
    if (Get-Command pwsh -ErrorAction SilentlyContinue) {
        pwsh -NoProfile -File $PSScriptRoot/generate-licenses.ps1
    }
    else {
        Write-Host "Skipping license generation (requires PowerShell 7+). Installer build continues."
    }
}

function BuildLeadAndItsFriends {
    Write-Output "Building LEAD release binaries for channel: $channel"
    if ($SkipBuild) {
        Write-Output "Skipping cargo build (-SkipBuild)"
        if (-not (Test-Path ".\$CargoOutDir\lead.exe")) {
            throw "Missing release binary: .\$CargoOutDir\lead.exe (remove -SkipBuild to compile)"
        }
    }
    else {
        # Limit parallel rustc invocations — full release LTO can OOM on 16 GB machines.
        $env:CARGO_BUILD_JOBS = "1"
        cargo build -j 1 --release --package cli --package auto_update_helper --target $target
        cargo build -j 1 --release --package LEAD --bin lead --target $target
    }
    Copy-Item -Path ".\$CargoOutDir\lead.exe" -Destination "$innoDir\LEAD.exe" -Force
    Copy-Item -Path ".\$CargoOutDir\cli.exe" -Destination "$innoDir\cli.exe" -Force
    Copy-Item -Path ".\$CargoOutDir\auto_update_helper.exe" -Destination "$innoDir\auto_update_helper.exe" -Force
}

function DownloadAMDGpuServices {
    $dll = ".\AGS_SDK-6.3.0\ags_lib\lib\amd_ags_x64.dll"
    if (Test-Path $dll) {
        return
    }
    $url = "https://codeload.github.com/GPUOpen-LibrariesAndSDKs/AGS_SDK/zip/refs/tags/v6.3.0"
    $zipPath = ".\AGS_SDK_v6.3.0.zip"
    if (-not (Test-Path $zipPath)) {
        Invoke-WebRequest -Uri $url -OutFile $zipPath
    }
    if (Test-Path ".\AGS_SDK-6.3.0") {
        Remove-Item -Path ".\AGS_SDK-6.3.0" -Recurse -Force
    }
    Expand-Archive -Path $zipPath -DestinationPath "." -Force
}

function DownloadConpty {
    $marker = ".\conpty\runtimes\win-x64\native\conpty.dll"
    if (Test-Path $marker) {
        return
    }
    $nupkgPath = ".\Microsoft.Windows.Console.ConPTY.1.23.251216003.nupkg"
    if (-not (Test-Path $nupkgPath)) {
        $url = "https://github.com/microsoft/terminal/releases/download/v1.23.13503.0/Microsoft.Windows.Console.ConPTY.1.23.251216003.nupkg"
        Invoke-WebRequest -Uri $url -OutFile $nupkgPath
    }
    if (Test-Path ".\conpty") {
        Remove-Item -Path ".\conpty" -Recurse -Force
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::ExtractToDirectory(
        (Resolve-Path $nupkgPath),
        (Join-Path (Get-Location) "conpty")
    )
}

function CollectFiles {
    Move-Item -Path "$innoDir\cli.exe" -Destination "$innoDir\bin\LEAD.exe" -Force
    Move-Item -Path "$innoDir\lead.sh" -Destination "$innoDir\bin\lead" -Force
    Move-Item -Path "$innoDir\auto_update_helper.exe" -Destination "$innoDir\tools\auto_update_helper.exe" -Force
    if ($Architecture -eq "aarch64") {
        New-Item -Type Directory -Path "$innoDir\arm64" -Force
        Move-Item -Path ".\conpty\build\native\runtimes\arm64\OpenConsole.exe" -Destination "$innoDir\arm64\OpenConsole.exe" -Force
        Move-Item -Path ".\conpty\runtimes\win-arm64\native\conpty.dll" -Destination "$innoDir\conpty.dll" -Force
    }
    else {
        New-Item -Type Directory -Path "$innoDir\x64" -Force
        New-Item -Type Directory -Path "$innoDir\arm64" -Force
        if (Test-Path ".\AGS_SDK-6.3.0\ags_lib\lib\amd_ags_x64.dll") {
            Copy-Item -Path ".\AGS_SDK-6.3.0\ags_lib\lib\amd_ags_x64.dll" -Destination "$innoDir\amd_ags_x64.dll" -Force
        }
        Move-Item -Path ".\conpty\build\native\runtimes\x64\OpenConsole.exe" -Destination "$innoDir\x64\OpenConsole.exe" -Force
        Move-Item -Path ".\conpty\build\native\runtimes\arm64\OpenConsole.exe" -Destination "$innoDir\arm64\OpenConsole.exe" -Force
        Move-Item -Path ".\conpty\runtimes\win-x64\native\conpty.dll" -Destination "$innoDir\conpty.dll" -Force
    }
}

function BuildInstaller {
    $issFilePath = "$innoDir\lead.iss"

    if (-not $env:RELEASE_VERSION) {
        $cargoToml = Get-Content "$env:ZED_WORKSPACE\crates\zed\Cargo.toml" -Raw
        if ($cargoToml -match '(?m)^version\s*=\s*"([^"]+)"') {
            $env:RELEASE_VERSION = $Matches[1]
        }
    }

    # LEAD-specific identifiers — must not overlap with Zed's AppId, mutex, registry, or AppUserModel IDs.
    $appId = "{{7F2A9E4C-1B8D-4F6A-A3E5-9D0C8B7A6E4F}"
    $appIconName = "app-icon-dev"
    $appName = "LEAD"
    $appDisplayName = "LEAD"
    $appSetupName = "LEAD-$env:RELEASE_VERSION-$Architecture"
    $appMutex = "LEAD-Editor-Dev-Instance-Mutex"
    $appExeName = "LEAD"
    $regValueName = "LEAD"
    $appUserId = "dev.lead.LEAD-Dev"
    $appShellNameShort = "L&EAD"

    $innoSetupPath = Get-InnoSetupPath

    $definitions = @{
        "AppId"          = $appId
        "AppIconName"    = $appIconName
        "OutputDir"      = "$env:ZED_WORKSPACE\target"
        "AppSetupName"   = $appSetupName
        "AppName"        = $appName
        "AppDisplayName" = $appDisplayName
        "RegValueName"   = $regValueName
        "AppMutex"       = $appMutex
        "AppExeName"     = $appExeName
        "ResourcesDir"   = "$innoDir"
        "ShellNameShort" = $appShellNameShort
        "AppUserId"      = $appUserId
        "Version"        = "$env:RELEASE_VERSION"
        "SourceDir"      = "$env:ZED_WORKSPACE"
    }

    $defs = @()
    foreach ($key in $definitions.Keys) {
        $defs += "/d$key=`"$($definitions[$key])`""
    }

    $innoArgs = @($issFilePath) + $defs
    Write-Host "Running Inno Setup: $innoSetupPath $($innoArgs -join ' ')"
    $process = Start-Process -FilePath $innoSetupPath -ArgumentList $innoArgs -NoNewWindow -Wait -PassThru

    if ($process.ExitCode -eq 0) {
        Write-Host "Inno Setup successfully compiled the LEAD installer"
        $script:buildSuccess = $true
        $script:installerPath = "$env:ZED_WORKSPACE\target\$appSetupName.exe"
    }
    else {
        Write-Host "Inno Setup failed with exit code $($process.ExitCode)"
        $script:buildSuccess = $false
    }
}

ParseZedWorkspace
$innoDir = "$env:ZED_WORKSPACE\inno\lead-$Architecture"

PrepareForBundle
GenerateLicenses
BuildLeadAndItsFriends
DownloadAMDGpuServices
DownloadConpty
CollectFiles
BuildInstaller

if ($buildSuccess) {
    Write-Output "LEAD installer built: $installerPath"
    if ($Install) {
        Write-Output "Launching LEAD installer..."
        Start-Process -FilePath $installerPath
    }
    exit 0
}
else {
    Write-Output "LEAD installer build failed"
    exit 1
}
