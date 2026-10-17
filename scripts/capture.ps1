# 抓主窗口截图，用来验证「播放器 + 日志面板」是否真的合到一个窗口里
param([string]$Out = "window.png")

Add-Type -AssemblyName System.Drawing
$dpi = @'
[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
'@
Add-Type -MemberDefinition $dpi -Name Dpi -Namespace Cap
# 关键：不声明 DPI 感知时，GetWindowRect/CopyFromScreen 都按 1.25 倍虚拟化，
# 截出来的图会被裁掉右下角（日志面板就看不见了）。
[Cap.Dpi]::SetProcessDPIAware() | Out-Null

$sig = @'
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
public struct RECT { public int Left, Top, Right, Bottom; }
'@
Add-Type -MemberDefinition $sig -Name Win -Namespace Cap

$p = Get-Process yuketang-autolearn -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { "no-window"; exit 1 }
$h = $p.MainWindowHandle
"pid=$($p.Id) title=$($p.MainWindowTitle)"

if ([Cap.Win]::IsIconic($h)) { [Cap.Win]::ShowWindow($h, 9) | Out-Null; Start-Sleep -Milliseconds 800 }
[Cap.Win]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 900

$r = New-Object Cap.Win+RECT
[Cap.Win]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.Right - $r.Left
$hh = $r.Bottom - $r.Top
"rect=$($r.Left),$($r.Top) ${w}x${hh}"

$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$bmp.Save((Join-Path (Get-Location) $Out), [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose()
"saved $Out"
