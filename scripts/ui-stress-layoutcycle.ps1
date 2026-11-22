# ui-stress-layoutcycle.ps1 — 布局循环(AG_E_LAYOUT_CYCLE)定向压测。
# 直开播放页(QAQ_BX_VIDEO)→ 评论区滚轮 10Hz + 窗口 ±1px 震荡 4Hz,
# N 分钟后报告进程存活与 data/crash-dumps 新增 dump 数。
# 用法: powershell -File scripts\ui-stress-layoutcycle.ps1 [-Seconds 240] [-Exe <路径>]
[CmdletBinding()]
param(
    [int]$Seconds = 240,
    [string]$Exe = ""
)
$ErrorActionPreference = "Stop"
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $RepoRoot "target\debug\qaq-bx-ui.exe" }
$DumpDir = Join-Path $RepoRoot "data\crash-dumps"
$LogPath = Join-Path $RepoRoot "target\ui-demo\ui-stress-layoutcycle.log"
New-Item -ItemType Directory -Force -Path (Split-Path $LogPath) | Out-Null

Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Runtime.InteropServices;
public static class StressNative {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc p, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr a, int X, int Y, int cx, int cy, uint f);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, int d, UIntPtr e);
    public const uint MOUSEEVENTF_WHEEL = 0x0800;
    public const uint SWP_NOZORDER = 0x0004;
}
"@
try { [void][StressNative]::SetProcessDpiAwarenessContext([IntPtr](-4)) } catch {}

function Write-Log([string]$m) {
    $line = "{0} {1}" -f (Get-Date -Format "HH:mm:ss"), $m
    $line | Tee-Object -FilePath $LogPath -Append
}

function Wait-MainWindow($proc, [int]$timeoutSec = 30) {
    $deadline = (Get-Date).AddSeconds($timeoutSec)
    $script:foundHwnd = [IntPtr]::Zero
    while ((Get-Date) -lt $deadline) {
        if ($proc.HasExited) { throw "进程提前退出 code=$($proc.ExitCode)" }
        $script:foundHwnd = [IntPtr]::Zero
        $cb = {
            param($h, $l)
            $pid2 = 0
            [void][StressNative]::GetWindowThreadProcessId($h, [ref]$pid2)
            if ($pid2 -eq $proc.Id -and [StressNative]::IsWindowVisible($h)) { $script:foundHwnd = $h; return $false }
            return $true
        }
        $null = [StressNative]::EnumWindows($cb, [IntPtr]::Zero)
        if ($script:foundHwnd -ne [IntPtr]::Zero) { return $script:foundHwnd }
        Start-Sleep -Milliseconds 250
    }
    throw "等待主窗口超时"
}

function Dump-Count {
    if (-not (Test-Path $DumpDir)) { return 0 }
    return (Get-ChildItem $DumpDir -Filter "*.dmp" -ErrorAction SilentlyContinue | Measure-Object).Count
}

$before = Dump-Count
Write-Log "== STRESS start exe=$Exe seconds=$Seconds dumpsBefore=$before =="
Remove-Item Env:QAQ_BX_NATIVE -ErrorAction SilentlyContinue
try { cmd /c "taskkill /IM qaq-bx-ui.exe /F >nul 2>&1" } catch {}
Start-Sleep -Milliseconds 500
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $Exe
$psi.WorkingDirectory = (Split-Path $Exe)
$psi.UseShellExecute = $true   # 脱离本会话管道,避免句柄继承挂住输出
$env:QAQ_BX_VIDEO = "BV1GJ411x7h7"   # UseShellExecute 只继承父进程环境
$proc = [System.Diagnostics.Process]::Start($psi)
$hwnd = Wait-MainWindow $proc
[void][StressNative]::SetForegroundWindow($hwnd)
Write-Log "main window hwnd=$hwnd pid=$($proc.Id)"
Start-Sleep -Seconds 8   # 播放起播 + 评论加载

$rect = New-Object StressNative+RECT
[void][StressNative]::GetWindowRect($hwnd, [ref]$rect)
$baseW = $rect.Right - $rect.Left; $baseH = $rect.Bottom - $rect.Top
$baseX = $rect.Left; $baseY = $rect.Top
$scrollX = [int]($baseX + $baseW * 0.30)   # 左列评论区
$scrollY = [int]($baseY + $baseH * 0.72)
Write-Log "window ${baseW}x${baseH} scroll-at($scrollX,$scrollY)"

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$i = 0
while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
    if ($proc.HasExited) {
        Write-Log "== RESULT == 进程在 $($sw.Elapsed.TotalSeconds.ToString('F1'))s 提前退出 code=$($proc.ExitCode) — 疑似崩溃复现!"
        $after = Dump-Count
        Write-Log "== dumps: before=$before after=$after (新增 $($after - $before)) =="
        exit 2
    }
    [void][StressNative]::SetCursorPos($scrollX, $scrollY)
    $dir = if ($i % 7 -lt 4) { -120 } else { 120 }   # 偏向下滚,偶尔回滚
    [StressNative]::mouse_event([StressNative]::MOUSEEVENTF_WHEEL, 0, 0, $dir, [UIntPtr]::Zero)
    if ($i % 4 -eq 0) {
        $dy = if (($i / 4) % 2 -eq 0) { 1 } else { -1 }
        [void][StressNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $baseX, $baseY, $baseW, $baseH + $dy, [StressNative]::SWP_NOZORDER)
    }
    $i++
    if ($i % 100 -eq 0) { Write-Log ("alive t={0:F0}s scrolls={1}" -f $sw.Elapsed.TotalSeconds, $i) }
    Start-Sleep -Milliseconds 100
}
if ($proc.HasExited) {
    Write-Log "== RESULT == 压测结束瞬间进程已退出 code=$($proc.ExitCode)"
    exit 2
}
Write-Log "== RESULT == SURVIVED ${Seconds}s scrolls=$i — 进程存活"
$after = Dump-Count
Write-Log "== dumps: before=$before after=$after (新增 $($after - $before)) =="
Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
exit 0
