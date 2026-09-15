param(
    [switch]$Run,
    [switch]$Release,
    [switch]$Lint,
    [string[]]$ImeArgs = @()
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = '0.1.0-m1'
Push-Location $root
try {
    # Check before compiling, rather than failing when rustc tries to replace a running EXE.
    if (@(Get-Process -Name imeflow -ErrorAction SilentlyContinue).Count -gt 0) {
        throw 'An imeflow.exe is running. Exit it from the tray first. No process was killed.'
    }
    $profile = if ($Release) { 'release' } else { 'debug' }
    $cargoArgs = @('test', '--locked')
    if ($Release) { $cargoArgs += '--release' }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'Unit tests failed; app was not started.' }

    if ($Lint) {
        $lintArgs = @('clippy', '--all-targets', '--locked')
        if ($Release) { $lintArgs += '--release' }
        $lintArgs += @('--', '-D', 'warnings')
        & cargo @lintArgs
        if ($LASTEXITCODE -ne 0) { throw 'Clippy failed; app was not started.' }
    }
    $buildArgs = @('build', '--locked')
    if ($Release) { $buildArgs += '--release' }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Build failed; app was not started.' }

    $bin = Join-Path $root ("target/{0}" -f $profile)
    Copy-Item -LiteralPath (Join-Path $root 'rules.toml') -Destination (Join-Path $bin 'rules.toml') -Force
    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $bin
    $exe = Join-Path $bin 'imeflow.exe'
    Write-Host 'Checking deployed PNG assets (no input hooks or IME changes)...'
    & $exe --check-assets
    if ($LASTEXITCODE -ne 0) { throw 'PNG validation failed; app was not started.' }
    Set-Content -LiteralPath (Join-Path $bin 'imeflow-build.id') -Value $version -Encoding ASCII
    Write-Host "Verified $version ($profile): $exe"

    if ($Run) {
        $diagnostics = Join-Path $root '.diagnostics'
        New-Item -ItemType Directory -Path $diagnostics -Force | Out-Null
        $log = Join-Path $diagnostics ('m1-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '.log')
        Write-Host "Log: $log"
        Write-Host 'Tray: auto / autostart / logs / exit'
        $previousLog = $env:RUST_LOG
        $previousDir = $env:IMEFLOW_LOG_DIR
        try {
            $env:RUST_LOG = 'info'
            $env:IMEFLOW_LOG_DIR = $diagnostics
            & $exe @ImeArgs 2>&1 | Tee-Object -FilePath $log
            if ($LASTEXITCODE -ne 0) { throw "ImeFlow exited with code $LASTEXITCODE; see $log" }
        }
        finally {
            $env:RUST_LOG = $previousLog
            $env:IMEFLOW_LOG_DIR = $previousDir
        }
    } else {
        Write-Host 'Verification complete. The IME automation was NOT started.'
    }
}
finally { Pop-Location }
