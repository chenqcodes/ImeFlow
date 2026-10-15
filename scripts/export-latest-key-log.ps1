$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'common.ps1')
$diagnostics = Get-ImeFlowLogDirectory
if (-not (Test-Path -LiteralPath $diagnostics)) { throw "Log directory not found: $diagnostics" }
$log = Get-ChildItem -LiteralPath $diagnostics -File -Filter 'session-*.log' |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($null -eq $log) {
    $fallback = Join-Path $diagnostics 'latest.log'
    if (Test-Path -LiteralPath $fallback -PathType Leaf) { $log = Get-Item -LiteralPath $fallback }
}
if ($null -eq $log) { throw 'No ImeFlow daemon session log found.' }
$out = Join-Path $diagnostics 'latest-key.log'
$pattern = 'build=m1|Cursor\.exe|Code\.exe|ChatGPT\.exe|Codex\.exe|Weixin\.exe|WeChat\.exe|WindowsTerminal\.exe|powershell\.exe|pwsh\.exe|cmd\.exe|conhost\.exe|msedge\.exe|chrome\.exe|firefox\.exe|address-bar|ime_apply|terminal:|focus quiet|terminal handoff|terminal guard|cursor terminal waiting|apply begin|apply end|apply stopped|shift:|key_trace|user override|flicker guard|UIA identity|focus did not stabilize'

Get-Content -LiteralPath $log.FullName |
    Where-Object { $_ -match $pattern } |
    Select-Object -Last 400 |
    Set-Content -LiteralPath $out -Encoding UTF8

Write-Host "Source: $($log.FullName)"
Write-Host "Saved : $out"
Write-Host 'You can now ask ChatGPT to read the exported latest-key.log path above.'
