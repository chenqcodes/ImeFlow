function Get-ImeFlowBuildId {
    (Get-Content -LiteralPath (Join-Path (Split-Path -Parent $PSScriptRoot) 'build-id.txt') -Raw).Trim()
}

function Get-ImeFlowLogDirectory {
    if (-not [string]::IsNullOrWhiteSpace($env:IMEFLOW_LOG_DIR)) { return $env:IMEFLOW_LOG_DIR }
    $base = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { [System.IO.Path]::GetTempPath() }
    Join-Path $base 'ImeFlow/logs'
}

function Get-ImeFlowBuildCandidates {
    param([string]$Root, [string]$Version)
    foreach ($relativeDirectory in @('target/debug', 'target/release', 'target/review-fixes/debug', 'target/review-fixes/release', 'target/rule-settings/debug', 'target/rule-settings/release')) {
        $directory = Join-Path $Root $relativeDirectory
        $executable = Join-Path $directory 'imeflow.exe'
        $stamp = Join-Path $directory 'imeflow-build.id'
        if ((Test-Path -LiteralPath $executable -PathType Leaf) -and (Test-Path -LiteralPath $stamp -PathType Leaf)) {
            if ((Get-Content -LiteralPath $stamp -Raw).Trim() -eq $Version) { Get-Item -LiteralPath $executable }
        }
    }
}

function Initialize-ImeFlowRules {
    param([string]$Root, [string]$Bin)
    $destination = Join-Path $Bin 'rules.toml'
    # Rule-panel edits belong to this instance. Rebuild/start must not erase them.
    if (-not (Test-Path -LiteralPath $destination)) {
        Copy-Item -LiteralPath (Join-Path $Root 'rules.toml') -Destination $destination
    }
}

function Invoke-ImeFlowProcess {
    param([string]$Exe, [string[]]$ImeArgs = @())
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Resolve-Path -LiteralPath $Exe).Path
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $ImeArgs) { $start.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        [pscustomobject]@{ ExitCode = $process.ExitCode; Output = $stdout.GetAwaiter().GetResult(); Error = $stderr.GetAwaiter().GetResult() }
    }
    finally { $process.Dispose() }
}

function Invoke-ImeFlowSession {
    param([string]$Exe, [string[]]$ImeArgs = @(), [string]$LogLevel = '')
    $logDirectory = Get-ImeFlowLogDirectory
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
    Write-Host "Logs: $logDirectory"
    Write-Host 'Current daemon: latest.log; history: session-*.log; probes: diagnostic-*.log'
    $previousFilter = $env:RUST_LOG
    $previousDirectory = $env:IMEFLOW_LOG_DIR
    try {
        # Preserve environment/.env configuration unless this caller explicitly overrides it.
        if ($LogLevel) { $env:RUST_LOG = $LogLevel }
        $env:IMEFLOW_LOG_DIR = $logDirectory
        $result = Invoke-ImeFlowProcess -Exe $Exe -ImeArgs $ImeArgs
        if ($result.Output) { Write-Host $result.Output.TrimEnd() }
        if ($result.Error) { Write-Host $result.Error.TrimEnd() }
        if ($result.ExitCode -ne 0) { throw "ImeFlow exited with code $($result.ExitCode). See: $logDirectory" }
    }
    finally {
        $env:RUST_LOG = $previousFilter
        $env:IMEFLOW_LOG_DIR = $previousDirectory
    }
}
