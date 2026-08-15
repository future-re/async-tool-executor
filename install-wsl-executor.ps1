<#
.SYNOPSIS
Builds and installs the WSL executor daemon from Windows by driving cargo
inside WSL through wsl.exe.

.DESCRIPTION
The guest daemon (wsl-executor-daemon) is a Linux binary and cannot be built
on Windows. This script invokes the cargo toolchain already present in the
target WSL distribution to produce and install it, then smoke-tests it.

All Linux-side paths are resolved to absolute paths: the WSL home directory is
queried up front (bash expands `~` from the passwd database) and any `~`
prefix in -GuestProgram is expanded to it. This avoids the broken $HOME
environment that wsl.exe --exec inherits from Windows.

.PARAMETER Distribution
WSL distribution to install into. Defaults to "Ubuntu".

.PARAMETER GuestProgram
Install path inside WSL. Defaults to "~/.local/bin/ate-daemon".

.PARAMETER InstallConfig
Copy crates/wsl-executor-daemon/config.example.json to ~/.config/ate/config.json
when no config exists yet.

.PARAMETER BuildDir
Optional CARGO_TARGET_DIR override. Defaults to the repository's target/
directory so existing Linux build artifacts are reused.

.EXAMPLE
.\install-wsl-executor.ps1
.\install-wsl-executor.ps1 -Distribution Ubuntu -InstallConfig
#>
[CmdletBinding()]
param(
    [string]$Distribution = "Ubuntu",
    [string]$GuestProgram = "~/.local/bin/ate-daemon",
    [switch]$InstallConfig,
    [string]$BuildDir
)

$ErrorActionPreference = "Stop"

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

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$drive = $root.Substring(0, 1).ToLower()
$wslRoot = "/mnt/$drive" + ($root.Substring(2) -replace '\\', '/')

if (-not (Test-Path -LiteralPath $root)) {
    throw "script root does not exist: $root"
}

& wsl.exe -d $Distribution --exec true 2>$null
if ($LASTEXITCODE -ne 0) {
    throw "WSL distribution '$Distribution' is not available. Run 'wsl.exe -l -v' to list installed distributions."
}

Write-Host "Checking Rust toolchain inside WSL ($Distribution) ..."
Invoke-Wsl "command -v cargo >/dev/null 2>&1 || { echo 'cargo not found in WSL; install Rust with: curl --proto ''=https'' --tlsv1.2 -sSf https://sh.rustup.rs | sh' >&2; exit 1; }"

Write-Host "Resolving WSL home directory ..."
$wslHome = (Invoke-Wsl "echo ~").Trim()
if (-not $wslHome.StartsWith("/")) {
    throw "could not resolve WSL home directory (got '$wslHome')"
}

if ($GuestProgram.StartsWith("~/") -or $GuestProgram -eq "~") {
    $GuestProgram = $GuestProgram -replace '^~', $wslHome
}

Write-Host "Building wsl-executor-daemon inside WSL ($Distribution) from $wslRoot ..."
if ($BuildDir) {
    Invoke-Wsl "cd '$wslRoot' && CARGO_TARGET_DIR='$BuildDir' cargo build --release -p wsl-executor-daemon"
} else {
    Invoke-Wsl "cd '$wslRoot' && cargo build --release -p wsl-executor-daemon"
}

if ($BuildDir) {
    $binary = "$BuildDir/release/wsl-executor-daemon"
} else {
    $binary = "target/release/wsl-executor-daemon"
}

Write-Host "Installing guest daemon to $GuestProgram ..."
Invoke-Wsl "mkdir -p '$wslHome/.local/bin' && install -m 755 '$binary' '$GuestProgram'"

if ($InstallConfig) {
    $configDest = "$wslHome/.config/ate/config.json"
    Write-Host "Installing default config to $configDest (skipped if present) ..."
    Invoke-Wsl "mkdir -p '$wslHome/.config/ate' && [ -f '$configDest' ] || cp '$wslRoot/crates/wsl-executor-daemon/config.example.json' '$configDest'"
}

Write-Host "Smoke-testing '$GuestProgram' ..."
Invoke-Wsl "'$GuestProgram' < /dev/null && echo 'daemon OK'"

Write-Host "Done. The Windows client connects with:"
Write-Host "  WslClientConfig { distribution: `"$Distribution`", guest_program: `"$GuestProgram`" }"
