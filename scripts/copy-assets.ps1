param([Parameter(Mandatory = $true)][string]$Bin)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$source = Join-Path $root 'assets'
$destination = Join-Path $Bin 'assets'
$required = @(
    'icons/imeflow_tray_zh.png',
    'icons/imeflow_tray_en.png',
    'icons/imeflow_tray_off.png',
    'icons/imeflow_tray_neutral.png'
)
foreach ($relative in $required) {
    $file = Join-Path $source $relative
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "Required PNG is missing: $file"
    }
}
foreach ($relative in $required) {
    $file = Join-Path $destination $relative
    New-Item -ItemType Directory -Path (Split-Path -Parent $file) -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $source $relative) -Destination $file -Force
}
# Drop stale toast assets from older builds (toast UI removed).
$staleToast = Join-Path $destination 'ui'
if (Test-Path -LiteralPath $staleToast -PathType Container) {
    Remove-Item -LiteralPath $staleToast -Recurse -Force
}
Write-Host "Assets copied: $destination"
