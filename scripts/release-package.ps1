param(
    [switch]$Install
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = '0.1.0-m1'
Push-Location $root
try {
    $running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        Write-Host 'Note: imeflow.exe is running (tray). Release build continues; install step needs you to exit first.'
    }

    Write-Host "=== ImeFlow release build ($version) ==="
    & cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Unit tests failed.' }
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

    $srcBin = Join-Path $root 'target\release'
    $dist = Join-Path $root 'dist\ImeFlow'
    if (Test-Path -LiteralPath $dist) {
        Remove-Item -LiteralPath $dist -Recurse -Force
    }
    New-Item -ItemType Directory -Path $dist -Force | Out-Null

    Copy-Item -LiteralPath (Join-Path $srcBin 'imeflow.exe') -Destination (Join-Path $dist 'imeflow.exe') -Force
    Copy-Item -LiteralPath (Join-Path $root 'rules.toml') -Destination (Join-Path $dist 'rules.toml') -Force
    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $dist

    $launch = @"
@echo off
cd /d "%~dp0"
start "" "%~dp0imeflow.exe"
"@
    Set-Content -LiteralPath (Join-Path $dist '启动 ImeFlow.cmd') -Value $launch -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $dist 'imeflow-build.id') -Value $version -Encoding ASCII

    & (Join-Path $dist 'imeflow.exe') --check-assets
    if ($LASTEXITCODE -ne 0) { throw 'PNG validation failed in dist package.' }

    Write-Host "Package ready: $dist"
    Write-Host 'Contents: imeflow.exe, rules.toml, assets/, 启动 ImeFlow.cmd'

    if ($Install) {
        & (Join-Path $PSScriptRoot 'install-local.ps1') -From $dist
    }
}
finally { Pop-Location }
