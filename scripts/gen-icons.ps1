# Draw connected 中E ligature (one spine; 中 and E join the bar) and tint 4 tray states.
Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'

$outDir = Join-Path $PSScriptRoot '..\assets\icons'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# Blue more vivid than original #2F6BFF. Idle gray lighter than the bar.
$Blue = [System.Drawing.Color]::FromArgb(255, 59, 130, 255)   # #3B82FF
$Bar = $Blue
$Gray = [System.Drawing.Color]::FromArgb(255, 201, 208, 218)  # #C9D0DA
$OffBar = [System.Drawing.Color]::FromArgb(255, 154, 163, 176) # #9AA3B0
$OffGray = [System.Drawing.Color]::FromArgb(255, 201, 208, 218)

function New-Stadium([float]$x, [float]$y, [float]$w, [float]$h) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [Math]::Min($w, $h)
    if ($d -le 0 -or $w -le 0 -or $h -le 0) { return $p }
    if ($w -ge $h) {
        $p.AddArc($x, $y, $d, $d, 90, 180)
        $p.AddArc($x + $w - $d, $y, $d, $d, 270, 180)
    } else {
        $p.AddArc($x, $y, $d, $d, 180, 180)
        $p.AddArc($x, $y + $h - $d, $d, $d, 0, 180)
    }
    $p.CloseFigure()
    return $p
}

function New-Mask {
    $S = 256
    $sw = 38
    $pad = 15
    $spineX = 109
    $boxTop = 42
    $boxBot = 213
    $midY = 109
    $right = $S - $pad

    $bmp = New-Object System.Drawing.Bitmap $S, $S, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $brush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)

    # Overlapping stadiums so T-junctions stay solid.
    $parts = @(
        (New-Stadium $spineX $pad $sw ($S - 2 * $pad)),                 # full spine
        (New-Stadium $pad $boxTop $sw ($boxBot - $boxTop)),             # 中 left upright
        (New-Stadium $pad $boxTop ($right - $pad) $sw),                 # 中+E top bar through spine
        (New-Stadium $spineX $midY ($right - $spineX) $sw),             # E middle, joined to spine
        (New-Stadium $pad ($boxBot - $sw) ($right - $pad) $sw)          # 中+E bottom bar through spine
    )
    foreach ($path in $parts) {
        $g.FillPath($brush, $path)
        $path.Dispose()
    }
    $brush.Dispose(); $g.Dispose()
    return @{
        Bitmap = $bmp
        SpineLo = [int]$spineX
        SpineHi = [int]($spineX + $sw)
        BoxTop = [int]$boxTop
        BoxBot = [int]$boxBot
    }
}

function Write-Tinted(
    [string]$name,
    $mask,
    [System.Drawing.Color]$left,
    [System.Drawing.Color]$spine,
    [System.Drawing.Color]$right,
    [System.Drawing.Color]$spineCap = [System.Drawing.Color]::Empty
) {
    $src = $mask.Bitmap
    $dst = New-Object System.Drawing.Bitmap $src.Width, $src.Height, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $lo = $mask.SpineLo
    $hi = $mask.SpineHi
    $dimCaps = $spineCap.A -ne 0
    for ($y = 0; $y -lt $src.Height; $y++) {
        for ($x = 0; $x -lt $src.Width; $x++) {
            $p = $src.GetPixel($x, $y)
            if ($p.A -lt 8) { continue }
            if ($x -ge $lo -and $x -le $hi) {
                # Caps above top bar / below bottom bar stay inactive when zh/en is lit.
                if ($dimCaps -and ($y -lt $mask.BoxTop -or $y -gt $mask.BoxBot)) { $c = $spineCap }
                else { $c = $spine }
            }
            elseif ($x -lt $lo) { $c = $left }
            else { $c = $right }
            $dst.SetPixel($x, $y, [System.Drawing.Color]::FromArgb([int]$p.A, [int]$c.R, [int]$c.G, [int]$c.B))
        }
    }
    $path = Join-Path $outDir $name
    $dst.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $dst.Dispose()
    Write-Host "wrote $name"
}

$mask = New-Mask
Write-Tinted 'imeflow.png' $mask $Blue $Bar $Blue
# 中亮：整根竖线（含上下帽）都蓝；英亮：仅横档之间那段竖线蓝，上下帽灰。
Write-Tinted 'imeflow_zh.png' $mask $Blue $Bar $Gray
Write-Tinted 'imeflow_en.png' $mask $Gray $Bar $Blue $Gray
Write-Tinted 'imeflow_off.png' $mask $OffGray $OffBar $OffGray
$mask.Bitmap.Dispose()

foreach ($rootName in @('debug', 'release')) {
    $d = Join-Path $PSScriptRoot "..\target\$rootName\assets\icons"
    if (Test-Path (Split-Path $d)) {
        New-Item -ItemType Directory -Force -Path $d | Out-Null
        Copy-Item (Join-Path $outDir 'imeflow*.png') $d -Force
    }
}
Write-Host "icons in $outDir"
