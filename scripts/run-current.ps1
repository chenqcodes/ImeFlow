param([switch]$NoPause)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'common.ps1')
$version = Get-ImeFlowBuildId
try {
    if (@(Get-Process -Name imeflow -ErrorAction SilentlyContinue).Count -gt 0) {
        throw 'imeflow.exe is already running. Use tray Exit or Ctrl+C first.'
    }
    # Do not prefer an old release binary over a newly verified debug build.
    # Old versions ignore unknown CLI flags, so require the new build stamp before --check-assets.
    $candidates = @(Get-ImeFlowBuildCandidates -Root $root -Version $version)
    $selected = $candidates | Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if ($null -eq $selected) { throw 'No matching verified build found. Run 01-build-and-run.cmd first.' }
    $exe = $selected.FullName
    $bin = $selected.DirectoryName
    Initialize-ImeFlowRules -Root $root -Bin $bin
    & (Join-Path $PSScriptRoot 'copy-assets.ps1') -Bin $bin
    $assetCheck = Invoke-ImeFlowProcess -Exe $exe -ImeArgs @('--check-assets')
    if ($assetCheck.ExitCode -ne 0) { throw "Asset validation failed. The app was not started. $($assetCheck.Error)" }

    Write-Host "Starting ${version}: $exe"
    Invoke-ImeFlowSession -Exe $exe

}
catch {
    Write-Host ('ERROR: ' + $_.Exception.Message)
    if (-not $NoPause) { Read-Host 'Press Enter to close' }
    exit 1
}
