# 生成 assets/app.ico：蓝底白"学"字 + 播放三角，多尺寸打包成一个 ico
Add-Type -AssemblyName System.Drawing

$sizes = @(16, 24, 32, 48, 64, 128, 256)
$pngs = @()

foreach ($s in $sizes) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $g.Clear([System.Drawing.Color]::Transparent)

    # 圆角方块 + 竖向渐变
    $r = [Math]::Max(2, [int]($s * 0.22))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $w = $s - 1
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($w - $d, 0, $d, $d, 270, 90)
    $path.AddArc($w - $d, $w - $d, $d, $d, 0, 90)
    $path.AddArc(0, $w - $d, $d, $d, 90, 90)
    $path.CloseFigure()

    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        (New-Object System.Drawing.Point 0, 0),
        (New-Object System.Drawing.Point 0, $s),
        [System.Drawing.Color]::FromArgb(255, 74, 125, 255),
        [System.Drawing.Color]::FromArgb(255, 26, 58, 168))
    $g.FillPath($brush, $path)

    # 白字"学"
    $fontSize = [single]($s * 0.56)
    $font = New-Object System.Drawing.Font("Microsoft YaHei", $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $sf = New-Object System.Drawing.StringFormat
    $sf.Alignment = [System.Drawing.StringAlignment]::Center
    $sf.LineAlignment = [System.Drawing.StringAlignment]::Center
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    $rect = New-Object System.Drawing.RectangleF(0, ($s * -0.02), $s, $s)
    $g.DrawString("学", $font, $white, $rect, $sf)

    # 右下角播放三角（小尺寸时省略，否则糊成一团）
    if ($s -ge 32) {
        $cx = $s * 0.74
        $cy = $s * 0.75
        $rr = $s * 0.17
        $g.FillEllipse((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 255, 196, 0))), ($cx - $rr), ($cy - $rr), ($rr * 2), ($rr * 2))
        $tri = @(
            (New-Object System.Drawing.PointF(($cx - $rr * 0.38), ($cy - $rr * 0.5))),
            (New-Object System.Drawing.PointF(($cx - $rr * 0.38), ($cy + $rr * 0.5))),
            (New-Object System.Drawing.PointF(($cx + $rr * 0.55), $cy))
        )
        $g.FillPolygon((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 26, 58, 168))), $tri)
    }

    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , ($ms.ToArray())
    $g.Dispose(); $bmp.Dispose(); $ms.Dispose()
}

# 组装 ICO（Vista 起支持 PNG 压缩的条目）
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $out
$bw.Write([UInt16]0)          # reserved
$bw.Write([UInt16]1)          # type = icon
$bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]
    $dim = if ($s -ge 256) { 0 } else { $s }
    $bw.Write([Byte]$dim)     # width
    $bw.Write([Byte]$dim)     # height
    $bw.Write([Byte]0)        # colors
    $bw.Write([Byte]0)        # reserved
    $bw.Write([UInt16]1)      # planes
    $bw.Write([UInt16]32)     # bitcount
    $bw.Write([UInt32]$pngs[$i].Length)
    $bw.Write([UInt32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $bw.Write($p) }
$bw.Flush()

New-Item -ItemType Directory -Force -Path assets | Out-Null
[System.IO.File]::WriteAllBytes((Join-Path (Get-Location) "assets/app.ico"), $out.ToArray())
$bw.Dispose(); $out.Dispose()
"assets/app.ico : $((Get-Item assets/app.ico).Length) 字节"
