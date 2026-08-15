<#
.SYNOPSIS
Builds and installs the WSL executor daemon, with no Rust required inside WSL.

.DESCRIPTION
Installs wsl-executor-daemon into a WSL distribution using two build
strategies, selected automatically:

  native - compile inside WSL with the distribution's own cargo. Fastest, but
           requires the Rust toolchain to be installed in WSL. The build forces
           RUSTUP_TOOLCHAIN=stable so the repository's rust-toolchain.toml pin
           never forces a toolchain download inside WSL.

  cross  - compile a Linux binary from Windows using cargo-zigbuild (Zig as the
           cross linker). No Rust is needed inside WSL at all; the finished
           binary is copied in and installed. Cross builds default to a fully
           static musl binary so the daemon runs on any Linux distribution
           regardless of its glibc version.

BuildMode auto uses native when WSL already has cargo, otherwise falls back to
cross. Cross artifacts go to an isolated CARGO_TARGET_DIR so they never mix
with the repository's target/ (mixing Linux/Windows artifacts corrupts the
shared incremental cache and crashes proc-macro servers).

All Linux-side paths are resolved to absolute paths: the WSL home directory is
queried up front (bash expands `~` from the passwd database) and any `~`
prefix in -GuestProgram is expanded to it. This avoids the broken $HOME
environment that wsl.exe --exec inherits from Windows.

.PARAMETER Distribution
WSL distribution to install into. Defaults to "Ubuntu".

.PARAMETER GuestProgram
Install path inside WSL. Defaults to "~/.local/bin/ate-daemon".

.PARAMETER BuildMode
auto | native | cross. Defaults to auto.

.PARAMETER CrossTarget
musl (default) | gnu. musl produces a static binary; gnu links against the
distribution's glibc.

.PARAMETER InstallConfig
Copy crates/wsl-executor-daemon/config.example.json to ~/.config/ate/config.json
when no config exists yet.

.PARAMETER InstallTools
Allow the script to install missing Windows-side tools (cargo-zigbuild) when
cross-compiling. Without this, the script only reports the command to run.

.PARAMETER BuildDir
Override the build directory. For native builds this is a Linux path inside
WSL (default: ~/.cache/ate/target). For cross builds it is a Windows path
(default: %LOCALAPPDATA%\ate-cross-target).

.PARAMETER SkipClient
Skip building and installing the Windows ate.exe tool-management CLI.

.PARAMETER ClientInstallDir
Windows destination for ate.exe. Defaults to %LOCALAPPDATA%\ate\bin.

.EXAMPLE
.\install-wsl-executor.ps1
.\install-wsl-executor.ps1 -Distribution Ubuntu -InstallConfig
.\install-wsl-executor.ps1 -BuildMode cross -InstallTools
#>
[CmdletBinding()]
param(
    [string]$Distribution = "Ubuntu",
    [string]$GuestProgram = "~/.local/bin/ate-daemon",
    [ValidateSet("auto", "native", "cross")]
    [string]$BuildMode = "auto",
    [ValidateSet("musl", "gnu")]
    [string]$CrossTarget = "musl",
    [switch]$InstallConfig,
    [switch]$InstallTools,
    [string]$BuildDir,
    [switch]$SkipClient,
    [string]$ClientInstallDir = "$env:LOCALAPPDATA\ate\bin"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Invoke-Wsl {
    param(
        [Parameter(Mandatory)]
        [string]$Command
    )
    & wsl.exe -d $Distribution --exec bash -lc $Command
    if ($LASTEXITCODE -ne 0) {
        throw "wsl.exe exited with code ${LASTEXITCODE}: $Command"
    }
}

function Get-WslHome {
    $h = (Invoke-Wsl "echo ~").Trim()
    if (-not $h.StartsWith("/")) {
        throw "could not resolve WSL home directory (got '$h')"
    }
    return $h
}

function ConvertTo-WslPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )
    if ($Path -match '^[A-Za-z]:') {
        $drive = $Path.Substring(0, 1).ToLower()
        $rest = $Path.Substring(2) -replace '\\', '/'
        return "/mnt/$drive$rest"
    }
    if ($Path.StartsWith("\\\\")) {
        throw "UNC paths are not supported by wsl.exe: $Path"
    }
    throw "expected an absolute Windows path, got: $Path"
}

function Find-ZigRoot {
    $dir = Join-Path $env:LOCALAPPDATA "zig"
    return Get-ChildItem -Path $dir -Directory -ErrorAction SilentlyContinue |
        Where-Object { Test-Path (Join-Path $_.FullName "zig.exe") } |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

function Install-Zig {
    $zigDir = Join-Path $env:LOCALAPPDATA "zig"
    Write-Host "Downloading Zig (cross linker) ..."
    $index = Invoke-RestMethod -Uri "https://ziglang.org/download/index.json" -TimeoutSec 60
    $version = ($index.PSObject.Properties |
        Where-Object { $_.Name -ne "master" } |
        Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1).Name
    if (-not $version) { throw "could not determine latest Zig version from ziglang.org" }
    $entry = $index.$version."x86_64-windows"
    if (-not $entry -or -not $entry.tarball) {
        throw "no x86_64-windows build for Zig $version"
    }

    $zip = Join-Path $env:TEMP "zig-$version.zip"
    Write-Host "Downloading Zig $version from $($entry.tarball) ..."
    Invoke-WebRequest -Uri $entry.tarball -OutFile $zip -TimeoutSec 300
    New-Item -ItemType Directory -Path $zigDir -Force | Out-Null
    Expand-Archive -Path $zip -DestinationPath $zigDir -Force

    $zigRoot = Get-ChildItem -Path $zigDir -Directory |
        Where-Object { Test-Path (Join-Path $_.FullName "zig.exe") } |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $zigRoot) { throw "zig.exe not found after extracting $zip" }
    Write-Host "Installed Zig $version at $($zigRoot.FullName)"
    $env:PATH = "$($zigRoot.FullName);$env:PATH"
}

function Test-ElfBinary {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )
    $fs = [System.IO.File]::OpenRead($Path)
    try {
        $magic = New-Object byte[] 4
        $read = $fs.Read($magic, 0, 4)
        return ($read -eq 4 -and $magic[0] -eq 0x7f -and $magic[1] -eq 0x45 -and $magic[2] -eq 0x4c -and $magic[3] -eq 0x46)
    }
    finally {
        $fs.Dispose()
    }
}

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not (Test-Path -LiteralPath $root)) {
    throw "script root does not exist: $root"
}
$wslRoot = "/mnt/$($root.Substring(0, 1).ToLower())" + ($root.Substring(2) -replace '\\', '/')

Write-Host "Checking WSL distribution '$Distribution' ..."
& wsl.exe -d $Distribution --exec true 2>$null
if ($LASTEXITCODE -ne 0) {
    throw "WSL distribution '$Distribution' is not available. Run 'wsl.exe -l -v' to list installed distributions."
}

$wslHome = Get-WslHome
if ($GuestProgram.StartsWith("~/") -or $GuestProgram -eq "~") {
    $GuestProgram = $GuestProgram -replace '^~', $wslHome
}
if (-not $GuestProgram.StartsWith("/")) {
    throw "GuestProgram must be an absolute Linux path inside WSL, got: $GuestProgram"
}

$wslCargo = $true
try {
    Invoke-Wsl "command -v cargo >/dev/null 2>&1 || exit 1"
}
catch {
    $wslCargo = $false
}

$mode = $BuildMode
if ($mode -eq "auto") {
    if ($wslCargo) { $mode = "native" } else { $mode = "cross" }
}
Write-Host "Build mode: $mode (WSL cargo present: $wslCargo)"

switch ($mode) {
    "native" {
        if (-not $wslCargo) {
            throw "native build requested but cargo is not installed in WSL '$Distribution'. Install Rust with: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh  (or use -BuildMode cross to build from Windows)"
        }
        if (-not $BuildDir) { $BuildDir = "$wslHome/.cache/ate/target" }
        if (-not $BuildDir.StartsWith("/")) { throw "BuildDir must be an absolute Linux path inside WSL, got: $BuildDir" }

        Write-Host "Building wsl-executor-daemon inside WSL ($Distribution) ..."
        Invoke-Wsl "cd '$wslRoot' && CARGO_TARGET_DIR='$BuildDir' RUSTUP_TOOLCHAIN=stable cargo build --release -p wsl-executor-daemon"
        $binary = "$BuildDir/release/wsl-executor-daemon"
        Invoke-Wsl "od -An -tx1 -N4 '$binary' 2>/dev/null | grep -q '7f 45 4c 46' || { echo 'built binary is not an ELF executable: $binary' >&2; exit 1; }"
        $installSource = $binary
    }
    "cross" {
        Write-Host "Checking Windows-side Rust toolchain ..."
        if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
            throw "cargo not found on Windows. Install Rust with: https://rustup.rs"
        }
        if (-not (Get-Command cargo-zigbuild -ErrorAction SilentlyContinue)) {
            if ($InstallTools) {
                Write-Host "Installing cargo-zigbuild ..."
                cargo install cargo-zigbuild --locked
                if ($LASTEXITCODE -ne 0) { throw "cargo install cargo-zigbuild failed" }
            }
            else {
                throw "cargo-zigbuild is required for cross builds. Run 'cargo install cargo-zigbuild --locked' or re-run with -InstallTools."
            }
        }

        if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
            $existing = Find-ZigRoot
            if ($existing) {
                Write-Host "Using Zig from $($existing.FullName)"
                $env:PATH = "$($existing.FullName);$env:PATH"
            }
            elseif ($InstallTools) {
                Install-Zig
            }
            else {
                throw "zig is required for cross builds (cargo-zigbuild uses it as the cross linker). Re-run with -InstallTools to install it automatically, or install Zig from https://ziglang.org/download"
            }
        }

        $target = if ($CrossTarget -eq "musl") { "x86_64-unknown-linux-musl" } else { "x86_64-unknown-linux-gnu" }
        if (-not $BuildDir) {
            $BuildDir = Join-Path $env:LOCALAPPDATA "ate-cross-target"
        }
        elseif (-not [System.IO.Path]::IsPathRooted($BuildDir)) {
            $BuildDir = Join-Path $root $BuildDir
        }
        $BuildDir = [System.IO.Path]::GetFullPath($BuildDir)
        New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null

        Write-Host "Cross-compiling wsl-executor-daemon for $target (no Rust needed in WSL) ..."
        Write-Host "Ensuring Rust target '$target' is installed ..."
        rustup target add $target
        if ($LASTEXITCODE -ne 0) { throw "rustup target add $target failed" }

        $env:CARGO_TARGET_DIR = $BuildDir
        cargo zigbuild --release -p wsl-executor-daemon --target $target
        if ($LASTEXITCODE -ne 0) { throw "cargo zigbuild failed with exit code $LASTEXITCODE" }

        $binary = Join-Path $BuildDir "$target\release\wsl-executor-daemon"
        if (-not (Test-Path -LiteralPath $binary)) { throw "expected binary not found: $binary" }
        if (-not (Test-ElfBinary $binary)) { throw "built binary is not an ELF executable: $binary" }
        Write-Host "Built Linux ELF binary: $binary"

        $installSource = ConvertTo-WslPath $binary
    }
}

Write-Host "Installing guest daemon to $GuestProgram ..."
$guestDir = Split-Path -Parent $GuestProgram
Invoke-Wsl "mkdir -p '$guestDir' && install -m 755 '$installSource' '$GuestProgram'"

if ($InstallConfig) {
    $configDest = "$wslHome/.config/ate/config.json"
    $configSrc = ConvertTo-WslPath (Join-Path $root "crates\wsl-executor-daemon\config.example.json")
    Write-Host "Installing default config to $configDest (skipped if present) ..."
    Invoke-Wsl "mkdir -p '$wslHome/.config/ate' && [ -f '$configDest' ] || cp '$configSrc' '$configDest'"
}

Write-Host "Smoke-testing '$GuestProgram' ..."
Invoke-Wsl "'$GuestProgram' < /dev/null && echo 'daemon OK'"

if (-not $SkipClient) {
    $windowsCargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $windowsCargo) {
        throw "cargo was not found on Windows; install Rust or rerun with -SkipClient"
    }
    $clientBuildDir = Join-Path $env:LOCALAPPDATA "ate\build"
    Write-Host "Building Windows tool-management CLI ..."
    & cargo build --manifest-path (Join-Path $root "Cargo.toml") --release -p ate-cli --target-dir $clientBuildDir
    if ($LASTEXITCODE -ne 0) {
        throw "building ate-cli on Windows failed with code $LASTEXITCODE"
    }
    New-Item -ItemType Directory -Force -Path $ClientInstallDir | Out-Null
    Copy-Item -Force (Join-Path $clientBuildDir "release\ate.exe") (Join-Path $ClientInstallDir "ate.exe")
    Write-Host "Installed Windows CLI to $(Join-Path $ClientInstallDir 'ate.exe')"
}

Write-Host "Done. The Windows client connects with:"
Write-Host "  WslClientConfig { distribution: `"$Distribution`", guest_program: `"$GuestProgram`" }"
if (-not $SkipClient) {
    Write-Host "Add '$ClientInstallDir' to PATH to run: ate tool list --distribution $Distribution"
}
