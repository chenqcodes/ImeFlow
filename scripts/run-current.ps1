param([switch]$NoPause)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
try {
    if (@(Get-Process -Name imeflow -ErrorAction SilentlyContinue).Count -gt 0) {
        throw 'imeflow.exe is already running. Use tray Exit or Ctrl+C first.'
    }
    # Do not prefer an old release binary over a newly verified debug build.
    # Old versions ignore unknown CLI flags, so require the new build stamp before --check-assets.
    $candidates = @()
    foreach ($profile in @('debug', 'release')) {
        $dir = Join-Path $root ("target/{0}" -f $profile)
        $file = Join-Path $dir 'imeflow.exe'
        $stamp = Join-Path $dir 'imeflow-build.id'
        if ((Test-Path -LiteralPath $file -PathType Leaf) -and (Test-Path -LiteralPath $stamp -PathType Leaf)) {
            if ((Get-Content -LiteralPath $stamp -Raw).Trim() -eq 'm1.1-assets1') {
                $candidates += Get-Item -LiteralPath $file
            }
        }
    }
    $selected = $candidates | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($null -eq $selected) { throw 'No assets1-verified build found. Run 01-build-and-run.cmd first.' }
    $exe = $selected.FullName
    $bin = $selected.DirectoryName
    Copy-Item -LiteralPath (Join-Path $root 'rules.toml') -Destination (Join-Path $bin 'rules.toml') -Force
    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $bin
    & $exe --check-assets
    if ($LASTEXITCODE -ne 0) { throw 'Asset validation failed. The app was not started.' }

    $diagnostics = Join-Path $root '.diagnostics'
    New-Item -ItemType Directory -Path $diagnostics -Force | Out-Null
    $log = Join-Path $diagnostics ('m1-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '.log')
    Write-Host "Starting m1.1-assets1: $exe"
    Write-Host "Log: $log"
    $previousLog = $env:RUST_LOG
    $previousDir = $env:IMEFLOW_LOG_DIR
    try {
        $env:RUST_LOG = 'info'
        $env:IMEFLOW_LOG_DIR = $diagnostics
        & $exe 2>&1 | Tee-Object -FilePath $log
        $code = $LASTEXITCODE
    }
    finally {
        $env:RUST_LOG = $previousLog
        $env:IMEFLOW_LOG_DIR = $previousDir
    }
    if ($code -ne 0) { throw "ImeFlow exited with code $code. See: $log" }
}
catch {
    Write-Host ('ERROR: ' + $_.Exception.Message)
    if (-not $NoPause) { Read-Host 'Press Enter to close' }
    exit 1
}
