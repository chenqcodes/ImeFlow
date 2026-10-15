param([string]$Exe = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
$root = Split-Path -Parent $PSScriptRoot
$fixture = Join-Path $root ('target/tooling-check-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixture -Force | Out-Null
function Assert-Check { param([bool]$Condition, [string]$Message) if (-not $Condition) { throw $Message } }

$version = Get-ImeFlowBuildId
foreach ($item in @(@('debug', $version), @('release', 'old-build'), @('review-fixes/debug', $version))) {
    $directory = Join-Path $fixture ('target/' + $item[0])
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $directory 'imeflow.exe') -Value 'fixture; never executed'
    Set-Content -LiteralPath (Join-Path $directory 'imeflow-build.id') -Value $item[1]
}
$candidates = @(Get-ImeFlowBuildCandidates -Root $fixture -Version $version)
Assert-Check ($candidates.Count -eq 2) 'Version filtering failed.'
Assert-Check (@($candidates | Where-Object { $_.DirectoryName -like '*release' }).Count -eq 0) 'Old build accepted.'

$rulesBin = Join-Path $fixture 'target/debug'
Set-Content -LiteralPath (Join-Path $fixture 'rules.toml') -Value 'DEFAULT_RULES'
Initialize-ImeFlowRules -Root $fixture -Bin $rulesBin
Assert-Check ((Get-Content -LiteralPath (Join-Path $rulesBin 'rules.toml') -Raw).Trim() -eq 'DEFAULT_RULES') 'Missing rules were not initialized.'
Set-Content -LiteralPath (Join-Path $rulesBin 'rules.toml') -Value 'USER_OVERRIDE'
Initialize-ImeFlowRules -Root $fixture -Bin $rulesBin
Assert-Check ((Get-Content -LiteralPath (Join-Path $rulesBin 'rules.toml') -Raw).Trim() -eq 'USER_OVERRIDE') 'Saved user rules were overwritten.'

# Installer uses the same non-overwriting initializer with the release package as Root.
$installBin = Join-Path $fixture 'installed-app'
New-Item -ItemType Directory -Path $installBin -Force | Out-Null
Initialize-ImeFlowRules -Root $fixture -Bin $installBin
Assert-Check ((Get-Content -LiteralPath (Join-Path $installBin 'rules.toml') -Raw).Trim() -eq 'DEFAULT_RULES') 'First install lost defaults.'
Set-Content -LiteralPath (Join-Path $installBin 'rules.toml') -Value 'INSTALLED_USER_RULES'
Initialize-ImeFlowRules -Root $fixture -Bin $installBin
Assert-Check ((Get-Content -LiteralPath (Join-Path $installBin 'rules.toml') -Raw).Trim() -eq 'INSTALLED_USER_RULES') 'Upgrade overwrote installed rules.'

& (Join-Path $PSScriptRoot 'export-dependency-licenses.ps1') -Destination $fixture
$notices = Get-Content -LiteralPath (Join-Path $fixture 'THIRD_PARTY_NOTICES.txt') -Raw
Assert-Check ($notices.Contains('unicode-ident') -and $notices.Contains('Unicode-3.0')) 'Unicode dependency notice missing.'
Assert-Check (@(Get-ChildItem -LiteralPath (Join-Path $fixture 'licenses') -Directory).Count -gt 0) 'Dependency license texts missing.'
Assert-Check (-not $notices.Contains($env:USERPROFILE)) 'Local source path leaked into dependency notices.'

$previousDirectory = $env:IMEFLOW_LOG_DIR
try {
    $env:IMEFLOW_LOG_DIR = Join-Path $fixture 'logs'
    New-Item -ItemType Directory -Path $env:IMEFLOW_LOG_DIR -Force | Out-Null
    $daemonLog = Join-Path $env:IMEFLOW_LOG_DIR 'session-test.log'
    Set-Content -LiteralPath $daemonLog -Value 'ime_apply apply begin EXPECTED_DAEMON_SESSION'
    Set-Content -LiteralPath (Join-Path $env:IMEFLOW_LOG_DIR 'diagnostic-newer.log') -Value 'ime_apply WRONG_DIAGNOSTIC'
    Set-Content -LiteralPath (Join-Path $env:IMEFLOW_LOG_DIR 'latest.log') -Value 'KEEP_DAEMON_LATEST'
    & (Join-Path $PSScriptRoot 'export-latest-key-log.ps1')
    $exported = Get-Content -LiteralPath (Join-Path $env:IMEFLOW_LOG_DIR 'latest-key.log') -Raw
    Assert-Check ($exported.Contains('EXPECTED_DAEMON_SESSION')) 'Real session not exported.'
    Assert-Check (-not $exported.Contains('WRONG_DIAGNOSTIC')) 'Diagnostic replaced daemon export.'

    if ($Exe) {
        $resolvedExe = (Resolve-Path -LiteralPath $Exe).Path
        $versionResult = Invoke-ImeFlowProcess -Exe $resolvedExe -ImeArgs @('--version')
        Assert-Check ($versionResult.ExitCode -eq 0 -and $versionResult.Output.Trim() -eq $version) 'Executable version differs from build-id.txt.'
        $assetResult = Invoke-ImeFlowProcess -Exe $resolvedExe -ImeArgs @('--check-assets')
        Assert-Check ($assetResult.ExitCode -eq 0) 'Asset validation failed.'
        Assert-Check ((Get-Content -LiteralPath (Join-Path $env:IMEFLOW_LOG_DIR 'latest.log') -Raw).Trim() -eq 'KEEP_DAEMON_LATEST') 'Diagnostic truncated daemon latest.log.'
        Assert-Check (@(Get-ChildItem -LiteralPath $env:IMEFLOW_LOG_DIR -Filter 'diagnostic-*.log').Count -ge 2) 'Diagnostic session was not created.'

        # Force a safe refusal: either we own the mutex or an existing daemon already does.
        $mutex = [System.Threading.Mutex]::new($false, 'Local\ImeFlow')
        $ownsMutex = $false
        try {
            try { $ownsMutex = $mutex.WaitOne(0) }
            catch [System.Threading.AbandonedMutexException] { $ownsMutex = $true }
            $refused = Invoke-ImeFlowProcess -Exe $resolvedExe
            Assert-Check ($refused.ExitCode -eq 2) 'Duplicate instance was not refused.'
            Assert-Check ((Get-Content -LiteralPath (Join-Path $env:IMEFLOW_LOG_DIR 'latest.log') -Raw).Trim() -eq 'KEEP_DAEMON_LATEST') 'Refused launch truncated daemon log.'
        }
        finally {
            if ($ownsMutex) { $mutex.ReleaseMutex() }
            $mutex.Dispose()
        }

        # Existing but invalid assets must produce the executable's failure code, not a stale cargo exit code.
        $invalidBin = Join-Path $fixture 'invalid-assets'
        New-Item -ItemType Directory -Path (Join-Path $invalidBin 'assets') -Force | Out-Null
        $invalidExe = Join-Path $invalidBin 'imeflow.exe'
        Copy-Item -LiteralPath $resolvedExe -Destination $invalidExe
        $invalidAssets = Invoke-ImeFlowProcess -Exe $invalidExe -ImeArgs @('--check-assets')
        Assert-Check ($invalidAssets.ExitCode -eq 1) 'GUI resource failure did not return exit code 1.'
    }
}
finally { $env:IMEFLOW_LOG_DIR = $previousDirectory }
Write-Host 'Tooling checks passed: build selection, session export, diagnostic/refusal isolation, executable version/assets and GUI failure exit code.'
Write-Host "Fixtures retained: $fixture"
