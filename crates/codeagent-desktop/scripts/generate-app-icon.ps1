param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot "..\assets")
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$ringColor = [System.Drawing.ColorTranslator]::FromHtml("#7658db")
$dotColor = [System.Drawing.ColorTranslator]::FromHtml("#a893ff")

function New-IconPngBytes([int]$size) {
    $bitmap = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
            $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $graphics.Clear([System.Drawing.Color]::Transparent)

            # Preserve the header BrandMark's proportions while using more of the icon canvas.
            $outerDiameter = $size * 0.82
            $strokeWidth = [Math]::Max(1.0, $outerDiameter * (2.0 / 18.0))
            $outerInset = ($size - $outerDiameter) / 2.0
            $pen = [System.Drawing.Pen]::new($ringColor, $strokeWidth)
            try {
                $graphics.DrawEllipse(
                    $pen,
                    [single]($outerInset + ($strokeWidth / 2.0)),
                    [single]($outerInset + ($strokeWidth / 2.0)),
                    [single]($outerDiameter - $strokeWidth),
                    [single]($outerDiameter - $strokeWidth)
                )
            }
            finally {
                $pen.Dispose()
            }

            $dotDiameter = $outerDiameter * (5.0 / 18.0)
            $dotInset = ($size - $dotDiameter) / 2.0
            $brush = [System.Drawing.SolidBrush]::new($dotColor)
            try {
                $graphics.FillEllipse(
                    $brush,
                    [single]$dotInset,
                    [single]$dotInset,
                    [single]$dotDiameter,
                    [single]$dotDiameter
                )
            }
            finally {
                $brush.Dispose()
            }
        }
        finally {
            $graphics.Dispose()
        }

        $stream = [System.IO.MemoryStream]::new()
        try {
            $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
            return $stream.ToArray()
        }
        finally {
            $stream.Dispose()
        }
    }
    finally {
        $bitmap.Dispose()
    }
}

$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
[System.IO.Directory]::CreateDirectory($resolvedOutput) | Out-Null

$images = foreach ($size in $sizes) {
    [PSCustomObject]@{ Size = $size; Bytes = (New-IconPngBytes $size) }
}

[System.IO.File]::WriteAllBytes(
    (Join-Path $resolvedOutput "app-icon.png"),
    ($images | Where-Object Size -eq 256).Bytes
)

$iconPath = Join-Path $resolvedOutput "app-icon.ico"
$stream = [System.IO.File]::Create($iconPath)
try {
    $writer = [System.IO.BinaryWriter]::new($stream)
    try {
        $writer.Write([uint16]0)
        $writer.Write([uint16]1)
        $writer.Write([uint16]$images.Count)

        $offset = 6 + (16 * $images.Count)
        foreach ($image in $images) {
            $dimension = if ($image.Size -eq 256) { 0 } else { $image.Size }
            $writer.Write([byte]$dimension)
            $writer.Write([byte]$dimension)
            $writer.Write([byte]0)
            $writer.Write([byte]0)
            $writer.Write([uint16]1)
            $writer.Write([uint16]32)
            $writer.Write([uint32]$image.Bytes.Length)
            $writer.Write([uint32]$offset)
            $offset += $image.Bytes.Length
        }

        foreach ($image in $images) {
            $writer.Write([byte[]]$image.Bytes)
        }
    }
    finally {
        $writer.Dispose()
    }
}
finally {
    $stream.Dispose()
}

Write-Host "Generated $iconPath and app-icon.png"
