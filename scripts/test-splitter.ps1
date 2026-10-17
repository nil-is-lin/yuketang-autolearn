# 验证分隔条能拖：恢复窗口 → 直接给分隔条投递鼠标消息 → 比较布局
$sig = @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr c, string cls, string win);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
public struct RECT { public int Left, Top, Right, Bottom; }
'@
Add-Type -MemberDefinition $sig -Name D -Namespace SplitTest

$p = Get-Process yuketang-autolearn | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { "no-window"; exit 1 }
$main = $p.MainWindowHandle

if ([SplitTest.D]::IsIconic($main)) {
    [SplitTest.D]::ShowWindow($main, 9) | Out-Null
    Start-Sleep -Milliseconds 1200
}
[SplitTest.D]::SetForegroundWindow($main) | Out-Null
Start-Sleep -Milliseconds 800

$split = [SplitTest.D]::FindWindowEx($main, [IntPtr]::Zero, "YuketangAutolearnSplit", $null)
if ($split -eq [IntPtr]::Zero) { "no-splitter"; exit 1 }

$r = New-Object SplitTest.D+RECT
[SplitTest.D]::GetWindowRect($split, [ref]$r) | Out-Null
$before = $r.Top
"拖动前 分隔条 y=$before (高度 $($r.Bottom - $r.Top))"

$x = [int](($r.Left + $r.Right) / 2)
[SplitTest.D]::SetCursorPos($x, $before + 2) | Out-Null
Start-Sleep -Milliseconds 250
[SplitTest.D]::SendMessageW($split, 0x0201, [IntPtr]1, [IntPtr]::Zero) | Out-Null   # WM_LBUTTONDOWN
Start-Sleep -Milliseconds 250

# 往上游 160px：日志面板应该变高
[SplitTest.D]::SetCursorPos($x, $before - 160) | Out-Null
Start-Sleep -Milliseconds 150
[SplitTest.D]::SendMessageW($split, 0x0200, [IntPtr]0, [IntPtr]::Zero) | Out-Null   # WM_MOUSEMOVE
Start-Sleep -Milliseconds 250
[SplitTest.D]::SendMessageW($split, 0x0202, [IntPtr]0, [IntPtr]::Zero) | Out-Null   # WM_LBUTTONUP
Start-Sleep -Milliseconds 500

[SplitTest.D]::GetWindowRect($split, [ref]$r) | Out-Null
$after = $r.Top
"拖动后 分隔条 y=$after"

if ($after -lt $before - 40) { "结果：可拖动，日志面板变高 $($before - $after)px" }
elseif ($after -gt $before + 40) { "结果：可拖动，日志面板变矮 $($after - $before)px" }
else { "结果：没有变化（拖动未生效）" }
