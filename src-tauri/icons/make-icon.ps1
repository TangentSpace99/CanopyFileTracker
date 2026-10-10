# Copyright (c) 2026 Dr.-Ing. Axel Schild
# SPDX-License-Identifier: MIT

# Renders the Canopy app icon (a tiny treemap) and packs it into icon.ico / icon.png.
# Usage: powershell -File make-icon.ps1
Add-Type -AssemblyName System.Drawing

function New-RoundedPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-IconPng([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0

    $bg = New-RoundedPath (8 * $s) (8 * $s) (240 * $s) (240 * $s) (54 * $s)
    $bgBrush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.PointF(0, 0)), (New-Object System.Drawing.PointF($size, $size)),
        [System.Drawing.Color]::FromArgb(255, 23, 32, 42), [System.Drawing.Color]::FromArgb(255, 10, 14, 19))
    $g.FillPath($bgBrush, $bg)

    # x, y, w, h, colour
    $tiles = @(
        @(44, 44, 96, 168, '#34d399'),
        @(150, 44, 62, 96, '#60a5fa'),
        @(150, 150, 62, 62, '#fbbf24')
    )
    foreach ($t in $tiles) {
        $path = New-RoundedPath ($t[0] * $s) ($t[1] * $s) ($t[2] * $s) ($t[3] * $s) ([Math]::Max(1.0, 16 * $s))
        $brush = New-Object System.Drawing.SolidBrush([System.Drawing.ColorTranslator]::FromHtml($t[4]))
        $g.FillPath($brush, $path)
    }
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    return ,$ms.ToArray()
}

$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$sizes = 16, 24, 32, 48, 64, 256
$images = @{}
foreach ($sz in $sizes) { $images[$sz] = New-IconPng $sz }

$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($out)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($sz in $sizes) {
    $dim = if ($sz -ge 256) { 0 } else { $sz }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$images[$sz].Length); $w.Write([uint32]$offset)
    $offset += $images[$sz].Length
}
foreach ($sz in $sizes) { $w.Write($images[$sz]) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $dir 'icon.ico'), $out.ToArray())
[System.IO.File]::WriteAllBytes((Join-Path $dir 'icon.png'), (New-IconPng 512))
"wrote icon.ico ($($out.Length) bytes) and icon.png"
