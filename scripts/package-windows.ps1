# Builds Stick-itt for Windows (x64) and zips it into dist/.
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
Copy-Item LICENSE, README.md $stage

$zip = "dist\Stick-itt-$Version-windows-x64.zip"
Compress-Archive -Path "$stage\*" -DestinationPath $zip -Force
$zip
