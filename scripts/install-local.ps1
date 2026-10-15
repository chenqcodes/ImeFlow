param(
    [Parameter(Mandatory = $true)][string]$From
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
if (-not (Test-Path -LiteralPath (Join-Path $From 'imeflow.exe') -PathType Leaf)) {
    throw "imeflow.exe not found in: $From"
}

$app = Join-Path $env:LOCALAPPDATA 'ImeFlow\app'
New-Item -ItemType Directory -Path $app -Force | Out-Null

# Refuse installation while any instance is running; never terminate user sessions.
$running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    throw 'imeflow.exe is running. Exit from the tray, then install again.'
}

Copy-Item -LiteralPath (Join-Path $From 'imeflow.exe') -Destination (Join-Path $app 'imeflow.exe') -Force
Initialize-ImeFlowRules -Root $From -Bin $app
foreach ($licenseItem in @('LICENSE', 'THIRD_PARTY_NOTICES.txt', 'licenses')) {
    $source = Join-Path $From $licenseItem
    if (Test-Path -LiteralPath $source) {
        if ($licenseItem -eq 'licenses') {
            $licenseDestination = Join-Path $app 'licenses'
            New-Item -ItemType Directory -Path $licenseDestination -Force | Out-Null
            Get-ChildItem -LiteralPath $source | Copy-Item -Destination $licenseDestination -Recurse -Force
        } else {
            Copy-Item -LiteralPath $source -Destination (Join-Path $app $licenseItem) -Force
        }
    }
}
if (Test-Path -LiteralPath (Join-Path $From 'README.md')) {
    Copy-Item -LiteralPath (Join-Path $From 'README.md') -Destination (Join-Path $app 'README.md') -Force
}
if (Test-Path -LiteralPath (Join-Path $From 'README.en.md')) {
    Copy-Item -LiteralPath (Join-Path $From 'README.en.md') -Destination (Join-Path $app 'README.en.md') -Force
}
if (Test-Path -LiteralPath (Join-Path $From 'docs/rules-guide.md')) {
    New-Item -ItemType Directory -Path (Join-Path $app 'docs') -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $From 'docs/rules-guide.md') -Destination (Join-Path $app 'docs/rules-guide.md') -Force
}
if (Test-Path -LiteralPath (Join-Path $app 'assets')) {
    Remove-Item -LiteralPath (Join-Path $app 'assets') -Recurse -Force
}
Copy-Item -LiteralPath (Join-Path $From 'assets') -Destination (Join-Path $app 'assets') -Recurse -Force
Copy-Item -LiteralPath (Join-Path $From '启动 ImeFlow.cmd') -Destination (Join-Path $app '启动 ImeFlow.cmd') -Force

$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
$shortcutPath = Join-Path $startMenu 'ImeFlow.lnk'
$shell = New-Object -ComObject WScript.Shell
$sc = $shell.CreateShortcut($shortcutPath)
$sc.TargetPath = Join-Path $app 'imeflow.exe'
$sc.WorkingDirectory = $app
$sc.WindowStyle = 7
$sc.Description = 'ImeFlow — IME follows focus'
$sc.Save()

Write-Host "Installed to: $app"
Write-Host "Start Menu: $shortcutPath"
Write-Host 'Autostart: use tray menu 「开机自动启动」 after first launch.'
Write-Host 'Starting ImeFlow...'
Start-Process -FilePath (Join-Path $app 'imeflow.exe') -WorkingDirectory $app -WindowStyle Hidden
