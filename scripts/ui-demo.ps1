<#
.SYNOPSIS
    biliplayRS 自动化 UI 演示与测试脚本（支持平滑缓动交互、逐场景截图与 60fps MP4 宣传片录制）。
.EXAMPLE
    # 跑一遍完整 UI 测试与逐场景截图（输出到 target/ui-demo/frames/）
    just demo

    # 录制 60fps 宣传片 MP4（输出到 target/ui-demo/promo.mp4）
    just demo-record
#>
[CmdletBinding()]
param(
    [switch]$Record,
    [switch]$SkipBuild,
    [int]$Fps = 60
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$OutRoot = Join-Path $RepoRoot "target\ui-demo"
$FramesDir = Join-Path $OutRoot "frames"
New-Item -ItemType Directory -Force -Path $FramesDir | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class UiAutomationNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int X, int Y);

    [DllImport("user32.dll")]
    public static extern bool GetCursorPos(out System.Drawing.Point lpPoint);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint dwFlags, int dx, int dy, int dwData, UIntPtr dwExtraInfo);

    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP   = 0x0004;
    public const uint MOUSEEVENTF_WHEEL    = 0x0800;
    public const int SW_RESTORE = 9;
}
"@

function EaseInOutCubic([double]$t) {
    if ($t -lt 0.5) { return 4.0 * $t * $t * $t }
    $f = (2.0 * $t) - 2.0
    return 0.5 * $f * $f * $f + 1.0
}

function Move-CursorSmooth {
    param([int]$TargetX, [int]$TargetY, [int]$DurationMs = 480)
    $start = New-Object System.Drawing.Point
    [void][UiAutomationNative]::GetCursorPos([ref]$start)
    $steps = [Math]::Max(1, [int]($DurationMs / 16))
    for ($i = 1; $i -le $steps; $i++) {
        $k = EaseInOutCubic($i / [double]$steps)
        $x = [int][Math]::Round($start.X + ($TargetX - $start.X) * $k)
        $y = [int][Math]::Round($start.Y + ($TargetY - $start.Y) * $k)
        [void][UiAutomationNative]::SetCursorPos($x, $y)
        Start-Sleep -Milliseconds 16
    }
}

function Click-Smooth {
    param([int]$X, [int]$Y, [int]$MoveMs = 450)
    Move-CursorSmooth -TargetX $X -TargetY $Y -DurationMs $MoveMs
    Start-Sleep -Milliseconds 120
    [UiAutomationNative]::mouse_event([UiAutomationNative]::MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 50
    [UiAutomationNative]::mouse_event([UiAutomationNative]::MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
}

function Scroll-Smooth {
    param([int]$TotalDelta, [int]$Steps = 18, [int]$StepMs = 24)
    $perStep = [int]($TotalDelta / $Steps)
    for ($i = 0; $i -lt $Steps; $i++) {
        [UiAutomationNative]::mouse_event([UiAutomationNative]::MOUSEEVENTF_WHEEL, 0, 0, $perStep, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds $StepMs
    }
}

function Capture-WindowFrame {
    param([IntPtr]$Hwnd, [string]$FileName)
    $rect = New-Object UiAutomationNative+RECT
    [void][UiAutomationNative]::GetWindowRect($Hwnd, [ref]$rect)
    $w = [Math]::Max(1, $rect.Right - $rect.Left)
    $h = [Math]::Max(1, $rect.Bottom - $rect.Top)
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
    $g.Dispose()
    $path = Join-Path $FramesDir $FileName
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "[shot] $path" -ForegroundColor DarkCyan
}

if (-not $SkipBuild) {
    Write-Host "[build] cargo build -p bili-ui" -ForegroundColor Cyan
    Push-Location $RepoRoot
    try {
        & cargo build -p bili-ui
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
    } finally {
        Pop-Location
    }
}

$exe = Join-Path $RepoRoot "target\debug\bili-ui.exe"
if (-not (Test-Path $exe)) { throw "未找到 $exe" }

Write-Host "[launch] 启动 bili-ui.exe..." -ForegroundColor Cyan
$proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
$ffmpegProc = $null

try {
    # 等待主窗口就绪
    $hwnd = [IntPtr]::Zero
    for ($i = 0; $i -lt 60; $i++) {
        $proc.Refresh()
        if ($proc.HasExited) { throw "bili-ui.exe 提前退出 ($($proc.ExitCode))" }
        if ($proc.MainWindowHandle -ne [IntPtr]::Zero) {
            $hwnd = $proc.MainWindowHandle
            break
        }
        Start-Sleep -Milliseconds 250
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "等待主窗口超时" }

    [void][UiAutomationNative]::ShowWindow($hwnd, [UiAutomationNative]::SW_RESTORE)
    [void][UiAutomationNative]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 1800 # 等待推荐流首屏与封面加载

    $rect = New-Object UiAutomationNative+RECT
    [void][UiAutomationNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left
    $winY = $rect.Top
    $winW = $rect.Right - $rect.Left
    $winH = $rect.Bottom - $rect.Top
    # gdigrab 要求偶数宽高
    $recW = $winW - ($winW % 2)
    $recH = $winH - ($winH % 2)

    if ($Record) {
        $ffmpegCmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
        if (-not $ffmpegCmd) { throw "未找到 ffmpeg.exe，无法录制 MP4" }
        $mp4Path = Join-Path $OutRoot "promo.mp4"
        Write-Host "[rec] 开始录制 ${Fps}fps 宣传片 -> $mp4Path" -ForegroundColor Yellow
        $ffmpegArgs = @(
            "-y",
            "-f", "gdigrab",
            "-framerate", "$Fps",
            "-offset_x", "$winX",
            "-offset_y", "$winY",
            "-video_size", "${recW}x${recH}",
            "-i", "desktop",
            "-c:v", "libx264",
            "-preset", "veryfast",
            "-pix_fmt", "yuv420p",
            "`"$mp4Path`""
        ) -join " "
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $ffmpegCmd.Source
        $psi.Arguments = $ffmpegArgs
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $psi.RedirectStandardInput = $true
        $ffmpegProc = [System.Diagnostics.Process]::Start($psi)
        Start-Sleep -Milliseconds 400
    }

    # 场景 1：首页推荐流首屏展示 + 卡片悬停缩放动画
    Capture-WindowFrame -Hwnd $hwnd -FileName "01-home.png"
    Move-CursorSmooth -TargetX ($winX + [int]($winW * 0.36)) -TargetY ($winY + 260) -DurationMs 550
    Start-Sleep -Milliseconds 850
    Capture-WindowFrame -Hwnd $hwnd -FileName "02-card-hover.png"

    # 场景 2：首页平滑下滑浏览（同时用于后续验证返回不回顶）
    Scroll-Smooth -TotalDelta -720 -Steps 22 -StepMs 24
    Start-Sleep -Milliseconds 600
    Capture-WindowFrame -Hwnd $hwnd -FileName "03-home-scrolled.png"

    # 场景 3：点击卡片进入播放页（或半窗预览）
    Click-Smooth -X ($winX + [int]($winW * 0.36)) -Y ($winY + 290) -MoveMs 450
    Start-Sleep -Milliseconds 2200
    Capture-WindowFrame -Hwnd $hwnd -FileName "04-player.png"

    # 场景 4：在播放页向下滚动进入评论区，触发右下角小窗
    Move-CursorSmooth -TargetX ($winX + [int]($winW * 0.35)) -TargetY ($winY + [int]($winH * 0.55)) -DurationMs 350
    Scroll-Smooth -TotalDelta -780 -Steps 22 -StepMs 24
    Start-Sleep -Milliseconds 900
    Capture-WindowFrame -Hwnd $hwnd -FileName "05-player-comments-pip.png"

    # 滚回顶部主画面
    Scroll-Smooth -TotalDelta 780 -Steps 20 -StepMs 22
    Start-Sleep -Milliseconds 600

    # 场景 5：点击标题栏返回按钮，验证首页保活不回顶
    Click-Smooth -X ($winX + 24) -Y ($winY + 24) -MoveMs 450
    Start-Sleep -Milliseconds 800
    Capture-WindowFrame -Hwnd $hwnd -FileName "06-back-home-preserved.png"

    # 场景 6：切换左侧导航到“设置页”（展示全局 Noto Sans SC 与新开关）
    Click-Smooth -X ($winX + 24) -Y ($winY + $winH - 42) -MoveMs 500
    Start-Sleep -Milliseconds 800
    Capture-WindowFrame -Hwnd $hwnd -FileName "07-settings.png"

    Write-Host "[done] UI 自动化演示/测试完成，截图已保存至 $FramesDir" -ForegroundColor Green
} finally {
    if ($ffmpegProc -and -not $ffmpegProc.HasExited) {
        Write-Host "[rec] 正在优雅结束 ffmpeg 并写入 MP4 索引..." -ForegroundColor Yellow
        try {
            $ffmpegProc.StandardInput.WriteLine("q")
            $ffmpegProc.StandardInput.Flush()
            [void]$ffmpegProc.WaitForExit(5000)
        } catch {}
        if (-not $ffmpegProc.HasExited) {
            Stop-Process -Id $ffmpegProc.Id -Force -ErrorAction SilentlyContinue
        }
    }
    if ($proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
}
