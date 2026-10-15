param(
    [switch]$Run,
    [switch]$Release,
    [switch]$Lint,
    [string]$TargetDir = '',
    [string[]]$ImeArgs = @()
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'common.ps1')
$version = Get-ImeFlowBuildId
Push-Location $root
try {
    $profile = if ($Release) { 'release' } else { 'debug' }
    $targetRoot = if ($TargetDir) { [System.IO.Path]::GetFullPath((Join-Path $root $TargetDir)) } else { Join-Path $root 'target' }
    $bin = Join-Path $targetRoot $profile
    $exe = Join-Path $bin 'imeflow.exe'
    # Check before compiling, rather than failing when rustc tries to replace a running EXE.
    $running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
    if (($Run -and $running.Count -gt 0) -or @($running | Where-Object { $_.Path -eq $exe }).Count -gt 0) {
        throw 'A conflicting imeflow.exe is running. Exit it from the tray first. No process was killed.'
    }
    $cargoArgs = @('test', '--locked', '--target-dir', $targetRoot)
    if ($Release) { $cargoArgs += '--release' }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw 'Unit tests failed; app was not started.' }

    if ($Lint) {
        $lintArgs = @('clippy', '--all-targets', '--locked', '--target-dir', $targetRoot)
        if ($Release) { $lintArgs += '--release' }
        $lintArgs += @('--', '-D', 'warnings')
        & cargo @lintArgs
        if ($LASTEXITCODE -ne 0) { throw 'Clippy failed; app was not started.' }
    }
    $buildArgs = @('build', '--locked', '--target-dir', $targetRoot)
    if ($Release) { $buildArgs += '--release' }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'Build failed; app was not started.' }

    Initialize-ImeFlowRules -Root $root -Bin $bin
    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $bin
    Write-Host 'Checking deployed PNG assets (no input hooks or IME changes)...'
    $assetCheck = Invoke-ImeFlowProcess -Exe $exe -ImeArgs @('--check-assets')
    if ($assetCheck.ExitCode -ne 0) { throw "PNG validation failed; app was not started. $($assetCheck.Error)" }
    Set-Content -LiteralPath (Join-Path $bin 'imeflow-build.id') -Value $version -Encoding ASCII
    Write-Host "Verified $version ($profile): $exe"

    if ($Run) {
        Invoke-ImeFlowSession -Exe $exe -ImeArgs $ImeArgs
    } else {
        Write-Host 'Verification complete. The IME automation was NOT started.'
    }
}
finally { Pop-Location }
