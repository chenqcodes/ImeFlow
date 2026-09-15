param(
    [Parameter(Mandatory = $true)][string]$From
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath (Join-Path $From 'imeflow.exe') -PathType Leaf)) {
    throw "imeflow.exe not found in: $From"
}

$app = Join-Path $env:LOCALAPPDATA 'ImeFlow\app'
New-Item -ItemType Directory -Path $app -Force | Out-Null

# Stop only if installing over a running copy from this exact folder.
$running = @(Get-Process -Name imeflow -ErrorAction SilentlyContinue)
if ($running.Count -gt 0) {
    throw 'imeflow.exe is running. Exit from the tray, then install again.'
}

Copy-Item -LiteralPath (Join-Path $From 'imeflow.exe') -Destination (Join-Path $app 'imeflow.exe') -Force
Copy-Item -LiteralPath (Join-Path $From 'rules.toml') -Destination (Join-Path $app 'rules.toml') -Force
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
Start-Process -FilePath (Join-Path $app 'imeflow.exe') -WorkingDirectory $app
