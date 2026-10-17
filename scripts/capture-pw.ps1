# 用 PrintWindow 直接抓窗口内容，不依赖窗口是否在最前面（避免截到别的程序）
param([string]$Out = "window-pw.png")

Add-Type -AssemblyName System.Drawing
$sig = @'
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
public struct RECT { public int Left, Top, Right, Bottom; }
'@
Add-Type -MemberDefinition $sig -Name Pw -Namespace Cap

$p = Get-Process yuketang-autolearn -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { "no-window"; exit 1 }
$h = $p.MainWindowHandle

$r = New-Object Cap.Pw+RECT
[Cap.Pw]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left
$hh = $r.Bottom - $r.Top
"pid=$($p.Id) rect=${w}x${hh}"

$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
# 2 = PW_RENDERFULLCONTENT，抓 DirectComposition（WebView2）内容必须带这个标志
$ok = [Cap.Pw]::PrintWindow($h, $hdc, 2)
$g.ReleaseHdc($hdc)
$bmp.Save((Join-Path (Get-Location) $Out), [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
"printwindow=$ok saved=$Out"
