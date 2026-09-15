param([switch]$NoPause)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
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

    $diagnostics = Join-Path $root '.diagnostics'
    New-Item -ItemType Directory -Path $diagnostics -Force | Out-Null
    $log = Join-Path $diagnostics ('terminal-once-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.log')

    Write-Host 'Starting ImeFlow experimental build...'
    Write-Host "EXE: $exe"
    Write-Host "LOG: $log"
    Write-Host 'Close this window or press Ctrl+C to stop ImeFlow.'
    Write-Host ''

    $previousLog = $env:RUST_LOG
    try {
        $env:RUST_LOG = 'debug'
        & $exe 2>&1 | Tee-Object -FilePath $log
        $code = $LASTEXITCODE
    }
    finally {
        $env:RUST_LOG = $previousLog
    }

    if ($code -ne 0) {
        throw "ImeFlow exited with code $code. See: $log"
    }
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
