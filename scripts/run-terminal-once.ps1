param([switch]$NoPause)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'common.ps1')
$bin = Join-Path $root 'target/terminal-once/debug'
$exe = Join-Path $bin 'imeflow.exe'
$rules = Join-Path $root 'rules.toml'
$targetRules = Join-Path $bin 'rules.toml'

try {
    if (-not (Test-Path -LiteralPath $exe)) {
        throw "Experimental build not found: $exe`nRun 01-build-and-run.cmd first."
    }

    $running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
    if ($running.Count -gt 0) {
        throw 'imeflow.exe is already running. Close its run window or press Ctrl+C there, then try again.'
    }

    Copy-Item -LiteralPath $rules -Destination $targetRules -Force

    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $bin
    Invoke-ImeFlowSession -Exe $exe -LogLevel 'debug'
}
catch {
    Write-Host ''
    Write-Host ('ERROR: ' + $_.Exception.Message) -ForegroundColor Red
    if (-not $NoPause) {
        Write-Host ''
        Read-Host 'Press Enter to close'
    }
    exit 1
}
