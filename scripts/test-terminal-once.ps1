param(
    [switch]$Run,
    # Forwarded to imeflow.exe (e.g. -ImeArgs '--trace-probe')
    [string[]]$ImeArgs = @()
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'common.ps1')
Push-Location $root
try {
    # Separate output directory: do not overwrite or restart the previous build.
    & cargo test --locked --target-dir target/terminal-once
    if ($LASTEXITCODE -ne 0) { throw 'Unit tests failed; experimental app was not started.' }

    & cargo build --locked --target-dir target/terminal-once
    if ($LASTEXITCODE -ne 0) { throw 'Build failed; experimental app was not started.' }

    $bin = Join-Path $root 'target/terminal-once/debug'
    Copy-Item -LiteralPath (Join-Path $root 'rules.toml') -Destination (Join-Path $bin 'rules.toml') -Force
    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $bin
    $exe = Join-Path $bin 'imeflow.exe'
    $assetCheck = Invoke-ImeFlowProcess -Exe $exe -ImeArgs @('--check-assets')
    if ($assetCheck.ExitCode -ne 0) { throw "Asset validation failed. $($assetCheck.Error)" }
    Write-Host "Built $(Get-ImeFlowBuildId) (terminal-once): $exe"

    if ($Run) {
        $running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
        if ($running.Count -gt 0) {
            throw 'An imeflow.exe is still running. Close it manually, then rerun this script. No process was killed.'
        }
        Invoke-ImeFlowSession -Exe $exe -ImeArgs $ImeArgs -LogLevel 'debug'
    }
}
finally { Pop-Location }
