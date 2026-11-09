$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$images = @()
foreach ($size in @(16, 24, 32, 48, 64, 128, 256)) {
    $canvas = [System.Drawing.Bitmap]::new(1024, 1024)
    $graphics = [System.Drawing.Graphics]::FromImage($canvas)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.ScaleTransform(4, 4)
    $halo = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#FFD43B'))
    $rim = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml('#E7A91A'), 5)
    $pointer = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#17243B'))
    $outline = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 9)
    $outline.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    try {
        $graphics.FillEllipse($halo, 15, 15, 220, 220)
        $graphics.DrawEllipse($rim, 15, 15, 220, 220)
        $points = [System.Drawing.PointF[]]@(
            [System.Drawing.PointF]::new(79, 53),
            [System.Drawing.PointF]::new(79, 180),
            [System.Drawing.PointF]::new(111, 153),
            [System.Drawing.PointF]::new(139, 211),
            [System.Drawing.PointF]::new(164, 199),
            [System.Drawing.PointF]::new(137, 143),
            [System.Drawing.PointF]::new(181, 139)
        )
        $graphics.DrawPolygon($outline, $points)
        $graphics.FillPolygon($pointer, $points)
        $bitmap = [System.Drawing.Bitmap]::new($size, $size)
        $scaled = [System.Drawing.Graphics]::FromImage($bitmap)
        $stream = [System.IO.MemoryStream]::new()
        try {
            $scaled.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $scaled.DrawImage($canvas, 0, 0, $size, $size)
            $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
            $images += [pscustomobject]@{ Size = $size; Bytes = $stream.ToArray() }
        }
        finally {
            $stream.Dispose()
            $scaled.Dispose()
            $bitmap.Dispose()
        }
    }
    finally {
        $halo.Dispose()
        $rim.Dispose()
        $pointer.Dispose()
        $outline.Dispose()
        $graphics.Dispose()
        $canvas.Dispose()
    }
}

$file = [System.IO.File]::Create((Join-Path $PSScriptRoot 'CursorHighlighter.ico'))
$writer = [System.IO.BinaryWriter]::new($file)
try {
    $writer.Write([uint16]0)
    $writer.Write([uint16]1)
    $writer.Write([uint16]$images.Count)
    $offset = 6 + 16 * $images.Count
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
