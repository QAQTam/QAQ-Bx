# Home-route resize storm probe (#4 验收补充):默认路由(首页)下拖框风暴,采 RESIZE-STORM 计数。
param([string]$Exe = "D:\QAQ-Bx\target\debug\qaq-bx-ui.exe")
$sig = @'
using System;
using System.Runtime.InteropServices;
public class W {
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int cx, int cy, uint f);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
'@
Add-Type -TypeDefinition $sig

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $Exe
$psi.WorkingDirectory = Split-Path $Exe
$psi.UseShellExecute = $false
$psi.RedirectStandardError = $true
$proc = [System.Diagnostics.Process]::Start($psi)
$sink = $proc.StandardError
$task = $sink.ReadToEndAsync()
$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 100; $i++) {
    Start-Sleep -Milliseconds 200
    $proc.Refresh()
    if (-not $proc.HasExited -and $proc.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $proc.MainWindowHandle; break }
}
if ($hwnd -eq [IntPtr]::Zero) { Write-Host "[home-storm] no window"; exit 1 }
Start-Sleep -Seconds 6   # 等首页 feed 落位
$r = New-Object W+RECT
[void][W]::GetWindowRect($hwnd, [ref]$r)
$baseW = $r.Right - $r.Left; $baseH = $r.Bottom - $r.Top; $x = $r.Left; $y = $r.Top
Write-Host "[home-storm] base ${baseW}x${baseH}"
for ($i = 1; $i -le 30; $i++) {
    if ($i % 2 -eq 1) { $w = [int]($baseW * 0.6); $h = [int]($baseH * 0.6) } else { $w = $baseW; $h = $baseH }
    [void][W]::SetWindowPos($hwnd, [IntPtr]::Zero, $x, $y, $w, $h, 0x0040)
    Start-Sleep -Milliseconds 55
}
Start-Sleep -Milliseconds 800
Write-Host "[home-storm] done"
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
if ($task.Wait(4000)) {
    $text = $task.Result
    $storms = [regex]::Matches($text, 'RESIZE-STORM n=(\d+) span_ms=(\d+) max_gap_ms=(\d+)')
    if ($storms.Count -eq 0) { Write-Host "[home-storm] NO STORM LINES" }
    foreach ($m in $storms) {
        Write-Host ("[home-storm] storm: n={0} span_ms={1} max_gap_ms={2}" -f $m.Groups[1].Value, $m.Groups[2].Value, $m.Groups[3].Value)
    }
}
