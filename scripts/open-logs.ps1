$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'common.ps1')
$directory = Get-ImeFlowLogDirectory
New-Item -ItemType Directory -Path $directory -Force | Out-Null
Start-Process -FilePath explorer.exe -ArgumentList $directory
