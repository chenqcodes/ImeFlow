param(
    [switch]$Run,
    # Forwarded to imeflow.exe (e.g. -ImeArgs '--trace-probe')
    [string[]]$ImeArgs = @()
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    # Separate output directory: do not overwrite or restart the previous build.
    & cargo test --locked --target-dir target/terminal-once
    if ($LASTEXITCODE -ne 0) { throw 'Unit tests failed; experimental app was not started.' }

    & cargo build --locked --target-dir target/terminal-once
    if ($LASTEXITCODE -ne 0) { throw 'Build failed; experimental app was not started.' }

    $bin = Join-Path $root 'target/terminal-once/debug'
    Copy-Item -LiteralPath (Join-Path $root 'rules.toml') -Destination (Join-Path $bin 'rules.toml') -Force
    $exe = Join-Path $bin 'imeflow.exe'
    Write-Host "Built terminal-once-v10: $exe"

    if ($Run) {
        $running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
        if ($running.Count -gt 0) {
            throw 'An imeflow.exe is still running. Close it manually, then rerun this script. No process was killed.'
        }
        $diagnostics = Join-Path $root '.diagnostics'
        New-Item -ItemType Directory -Path $diagnostics -Force | Out-Null
        $log = Join-Path $diagnostics ('terminal-once-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.log')
        Write-Host "Diagnostic log: $log"
        Write-Host 'Phase1: Shift LL watch is ON. Optional letter probe: -ImeArgs --trace-probe'
        Write-Host 'Select WeChat IME, then test chat -> integrated terminal; look for key_trace lines.'
        $previousLog = $env:RUST_LOG
        try {
            $env:RUST_LOG = 'debug'
            & $exe @ImeArgs 2>&1 | Tee-Object -FilePath $log
            if ($LASTEXITCODE -ne 0) { throw "ImeFlow exited with code $LASTEXITCODE; see $log" }
        }
        finally { $env:RUST_LOG = $previousLog }
    }
}
finally { Pop-Location }
