param([Parameter(Mandatory = $true)][string]$Destination)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$metadataJson = & cargo metadata --locked --offline --filter-platform x86_64-pc-windows-msvc --format-version 1 --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Dependency license metadata unavailable. Build the Windows release first.' }
$metadata = $metadataJson | ConvertFrom-Json
$licenseRoot = Join-Path $Destination 'licenses'
New-Item -ItemType Directory -Path $licenseRoot -Force | Out-Null
$index = [Collections.Generic.List[string]]::new()
$index.Add('Third-party Rust dependencies for the Windows x86_64 release')
$index.Add('License expressions and texts are supplied by the respective crate authors.')
$index.Add('')
foreach ($package in $metadata.packages | Where-Object source | Sort-Object name, version) {
    if (-not $package.license) { throw "Missing declared license: $($package.name)" }
    $directory = Split-Path -Parent $package.manifest_path
    $files = @(Get-ChildItem -LiteralPath $directory -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)([-._].*)?$' })
    if ($files.Count -eq 0) { throw "Missing license text: $($package.name)" }
    $relative = "$($package.name)-$($package.version)"
    $output = Join-Path $licenseRoot $relative
    New-Item -ItemType Directory -Path $output -Force | Out-Null
    foreach ($file in $files) { Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $output $file.Name) -Force }
    $index.Add("$($package.name) $($package.version): $($package.license) (licenses/$relative/)")
}
$index | Set-Content -LiteralPath (Join-Path $Destination 'THIRD_PARTY_NOTICES.txt') -Encoding utf8
Write-Host "Dependency licenses exported: $licenseRoot"
