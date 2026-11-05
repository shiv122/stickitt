# Builds Stick-itt for Windows (x64) into dist/:
#   Stick-itt-<version>-windows-portable.exe   the app itself, runs without installing
#   Stick-itt-<version>-windows-setup.exe      installer (needs Inno Setup 6; skipped locally if missing)
# Usage: scripts/package-windows.ps1 [-Version 0.1.0]   (default: the version in Cargo.toml)
param([string]$Version)
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not $Version) {
    $Version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
}

# Cargo reports progress on stderr, so check its exit code rather than stopping on stderr output.
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$ErrorActionPreference = 'Stop'

$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
$stage = 'target\package\Stick-itt'
Remove-Item -Recurse -Force target\package -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, dist | Out-Null
Copy-Item "$target\release\stickit.exe" "$stage\Stick-itt.exe"

$portable = "dist\Stick-itt-$Version-windows-portable.exe"
Copy-Item "$stage\Stick-itt.exe" $portable -Force
$portable

$iscc = @(
    $env:ISCC,
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
if (-not $iscc) {
    if ($env:CI) { throw 'Inno Setup (ISCC.exe) not found' }
    Write-Warning 'Inno Setup 6 not found: skipping the installer (portable exe only).'
    exit 0
}
& $iscc /Q "/DAppVersion=$Version" packaging\windows\stickitt.iss
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
"dist\Stick-itt-$Version-windows-setup.exe"
