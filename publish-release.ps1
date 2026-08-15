<#
.SYNOPSIS
Cross-compiles the WSL daemon from Windows and publishes it as a GitHub
Release asset for distribution.

.DESCRIPTION
Builds wsl-executor-daemon for x86_64-unknown-linux-musl and
aarch64-unknown-linux-musl from Windows using cargo-zigbuild (no Rust
toolchain needed inside WSL), packages each static binary into its own tar.gz
asset, and uploads them to a GitHub Release with gh.

The resulting assets are consumed by install-ate.ps1 -ReleaseUrl
(where the {arch} placeholder selects the right one) so other machines install
the prebuilt daemon without compiling or installing cargo-zigbuild / Zig
themselves. The Windows clients (ate.exe, ate-mcp.exe) are built and packaged
into a ate-windows-<tag>-<target>.zip asset consumed by install-ate.ps1
-ClientReleaseUrl. GitHub CI (.github/workflows/release.yml) does the same
build automatically on every v* tag push.

Requires on the Windows host:
  - Rust toolchain (cargo + rustup)
  - cargo-zigbuild (installed automatically with -InstallTools)
  - Zig (installed automatically with -InstallTools)
  - GitHub CLI (gh) authenticated to the target repository

.PARAMETER Tag
Release tag to create, e.g. "v0.1.0". Defaults to "v" + version from
Cargo.toml. If the tag already exists, the assets are uploaded to it.

.PARAMETER TargetRepo
GitHub repository, e.g. "owner/repo". Defaults to the repository's origin
remote URL.

.PARAMETER ReleaseNotes
Path to a file with the release body. Optional.

.PARAMETER InstallTools
Allow installing missing tools (cargo-zigbuild, Zig) automatically.

.PARAMETER BuildDir
Windows build directory for cross artifacts. Defaults to
%LOCALAPPDATA%\ate-release-target.

.PARAMETER OutDir
Where to write the tar.gz assets. Defaults to %LOCALAPPDATA%\ate-release.

.PARAMETER Targets
Comma-separated Rust targets to build. Defaults to
"x86_64-unknown-linux-musl,aarch64-unknown-linux-musl".

.EXAMPLE
.\publish-release.ps1 -Tag v0.1.0 -InstallTools
.\publish-release.ps1 -Tag v0.1.0 -ReleaseNotes .\CHANGELOG.md
#>
[CmdletBinding()]
param(
    [string]$Tag,
    [string]$TargetRepo,
    [string]$ReleaseNotes,
    [switch]$InstallTools,
    [string]$BuildDir,
    [string]$OutDir,
    [string]$Targets = "x86_64-unknown-linux-musl,aarch64-unknown-linux-musl"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not (Test-Path -LiteralPath $root)) {
    throw "script root does not exist: $root"
}

function Resolve-Repo {
    $origin = git -C $root remote get-url origin
    if ($origin -match 'github\.com[/:]([^/]+/[^/.]+)(\.git)?$') {
        return $matches[1]
    }
    throw "cannot determine GitHub repository from origin: $origin"
}

if (-not $TargetRepo) { $TargetRepo = Resolve-Repo }
if (-not $Tag) {
    $version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    $Tag = "v$version"
}
if (-not $BuildDir) { $BuildDir = Join-Path $env:LOCALAPPDATA "ate-release-target" }
if (-not $OutDir) { $OutDir = Join-Path $env:LOCALAPPDATA "ate-release" }

$targetList = @($Targets -split "," | ForEach-Object { $_.Trim() } | Where-Object { $_ })

# --- toolchain checks ------------------------------------------------------
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found on Windows. Install Rust from https://rustup.rs"
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
    if ($InstallTools) {
        Write-Host "Installing Zig via cargo-zigbuild setup ..."
        cargo zigbuild --help | Out-Null
        zig version | Out-Null
    }
    else {
        throw "zig is required for cross builds (cargo-zigbuild uses it as the cross linker). Re-run with -InstallTools or install Zig from https://ziglang.org/download"
    }
}
if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw "GitHub CLI (gh) is required to publish a Release. Install from https://cli.github.com and run 'gh auth login'."
}

New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
$env:CARGO_TARGET_DIR = $BuildDir
$assets = @()

foreach ($target in $targetList) {
    $assetBase = "ate-daemon-$Tag-$target"
    $assetPath = Join-Path $OutDir "$assetBase.tar.gz"

    # --- build -------------------------------------------------------------
    Write-Host "Cross-compiling wsl-executor-daemon for $target ..."
    rustup target add $target
    if ($LASTEXITCODE -ne 0) { throw "rustup target add $target failed" }
    cargo zigbuild --release -p wsl-executor-daemon --target $target
    if ($LASTEXITCODE -ne 0) { throw "cargo zigbuild failed with exit code $LASTEXITCODE" }

    $binary = Join-Path $BuildDir "$target\release\wsl-executor-daemon"
    if (-not (Test-Path -LiteralPath $binary)) { throw "expected binary not found: $binary" }

    # sanity: must be an ELF executable
    $fs = [System.IO.File]::OpenRead($binary)
    try {
        $magic = New-Object byte[] 4
        $read = $fs.Read($magic, 0, 4)
        $isElf = ($read -eq 4 -and $magic[0] -eq 0x7f -and $magic[1] -eq 0x45 -and $magic[2] -eq 0x4c -and $magic[3] -eq 0x46)
    }
    finally { $fs.Dispose() }
    if (-not $isElf) { throw "built binary is not an ELF executable: $binary" }

    # --- package --------------------------------------------------------------
    $stage = Join-Path $env:TEMP "ate-release-stage-$PID"
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    try {
        Copy-Item -Force $binary (Join-Path $stage "ate-daemon")
        Copy-Item -Force (Join-Path $root "crates\wsl-executor-daemon\config.example.json") (Join-Path $stage "config.example.json")
        Write-Host "Packaging $assetPath ..."
        tar -czf $assetPath -C $stage "ate-daemon" "config.example.json"
    }
    finally {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }
    if (-not (Test-Path -LiteralPath $assetPath)) { throw "failed to create $assetPath" }
    $bytes = (Get-Item -LiteralPath $assetPath).Length
    Write-Host "Created $assetPath ($([math]::Round($bytes/1MB, 2)) MB)"
    $assets += $assetPath
}

# --- Windows clients --------------------------------------------------------
$winArch = $env:PROCESSOR_ARCHITEW6432
if (-not $winArch) { $winArch = $env:PROCESSOR_ARCHITECTURE }
$winRustTarget = switch -Regex ($winArch) {
    "^(ARM64|Arm64|arm64)$" { "aarch64-pc-windows-msvc" }
    default                 { "x86_64-pc-windows-msvc" }
}
Write-Host "Building Windows clients (ate-cli, ate-mcp) for $winRustTarget ..."
cargo build --release -p ate-cli -p ate-mcp --target $winRustTarget
if ($LASTEXITCODE -ne 0) { throw "cargo build (Windows clients) failed with exit code $LASTEXITCODE" }

$winAssetBase = "ate-windows-$Tag-$winRustTarget"
$winAssetPath = Join-Path $OutDir "$winAssetBase.zip"
$stage = Join-Path $env:TEMP "ate-win-stage-$PID"
New-Item -ItemType Directory -Path $stage -Force | Out-Null
try {
    Copy-Item -Force (Join-Path $BuildDir "$winRustTarget\release\ate.exe") (Join-Path $stage "ate.exe")
    Copy-Item -Force (Join-Path $BuildDir "$winRustTarget\release\ate-mcp.exe") (Join-Path $stage "ate-mcp.exe")
    Write-Host "Packaging $winAssetPath ..."
    Compress-Archive -Path (Join-Path $stage "ate.exe"), (Join-Path $stage "ate-mcp.exe") -DestinationPath $winAssetPath -Force
}
finally {
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
}
if (-not (Test-Path -LiteralPath $winAssetPath)) { throw "failed to create $winAssetPath" }
$winBytes = (Get-Item -LiteralPath $winAssetPath).Length
Write-Host "Created $winAssetPath ($([math]::Round($winBytes/1MB, 2)) MB)"
$assets += $winAssetPath

# --- publish --------------------------------------------------------------
$notesArg = @()
if ($ReleaseNotes) {
    $notesArg = @("--notes-file", (Resolve-Path $ReleaseNotes))
}
$exists = gh release view $Tag --repo $TargetRepo 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Host "Creating release $Tag on $TargetRepo ..."
    gh release create $Tag $assets @notesArg --repo $TargetRepo --title $Tag
}
else {
    Write-Host "Release $Tag already exists; uploading assets ..."
    gh release upload $Tag $assets --repo $TargetRepo --clobber
}
if ($LASTEXITCODE -ne 0) { throw "gh release failed with exit code $LASTEXITCODE" }

Write-Host ""
Write-Host "Published:"
Write-Host "  repo:   $TargetRepo"
Write-Host "  tag:    $Tag"
foreach ($asset in $assets) { Write-Host "  asset:  $asset" }
Write-Host ""
Write-Host "Install on another machine with:"
Write-Host "  .\install-ate.ps1 -Repo $TargetRepo -ReleaseTag $Tag"
