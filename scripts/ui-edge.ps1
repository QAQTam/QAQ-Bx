<#
.SYNOPSIS
    QAQ-Bx 极端场景走查：访客态（无本地凭据）与快速连续切视频。
.EXAMPLE
    .\scripts\ui-edge.ps1            # 构建并跑两场景，截图输出 target/ui-demo/frames-edge/
    .\scripts\ui-edge.ps1 -SkipBuild # 跳过构建
#>
[CmdletBinding()]
param([switch]$SkipBuild)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$OutRoot = Join-Path $RepoRoot "target\ui-demo"
$FramesDir = Join-Path $OutRoot "frames-edge"
New-Item -ItemType Directory -Force -Path $FramesDir | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Runtime.InteropServices;

public static class UiEdgeNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, int dx, int dy, int dwData, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP = 0x0004;
    public const uint MOUSEEVENTF_WHEEL = 0x0800;
    public const uint SWP_NOZORDER = 0x0004;
    public const int SW_RESTORE = 9;
}
"@

function Capture-WindowFrame {
    param([IntPtr]$Hwnd, [string]$FileName)
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($Hwnd, [ref]$rect)
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

function Click-Fast {
    param([int]$X, [int]$Y)
    [void][UiEdgeNative]::SetCursorPos($X, $Y)
    Start-Sleep -Milliseconds 90
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
}

function Drag-WindowEdge {
    param([int]$StartX, [int]$StartY, [int]$EndX, [int]$EndY)
    [void][UiEdgeNative]::SetCursorPos($StartX, $StartY)
    Start-Sleep -Milliseconds 120
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 80
    $steps = 16
    for ($i = 1; $i -le $steps; $i++) {
        $x = [int][Math]::Round($StartX + ($EndX - $StartX) * ($i / [double]$steps))
        $y = [int][Math]::Round($StartY + ($EndY - $StartY) * ($i / [double]$steps))
        [void][UiEdgeNative]::SetCursorPos($x, $y)
        Start-Sleep -Milliseconds 18
    }
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 350
}

function Scroll-Fast {
    param([int]$TotalDelta)
    for ($i = 0; $i -lt 12; $i++) {
        [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_WHEEL, 0, 0, [int]($TotalDelta / 12), [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 22
    }
}

if (-not $SkipBuild) {
    Write-Host "[build] cargo build -p qaq-bx-ui" -ForegroundColor Cyan
    Push-Location $RepoRoot
    try {
        & cargo build -p qaq-bx-ui
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
    } finally { Pop-Location }
}

$exe = Join-Path $RepoRoot "target\debug\qaq-bx-ui.exe"
if (-not (Test-Path $exe)) { throw "未找到 $exe" }
$dataDir = Join-Path $RepoRoot "target\debug\data"
$credPath = Join-Path $dataDir "credentials.json"
$credBackup = "$credPath.edge-bak"

function Wait-MainWindow {
    param([System.Diagnostics.Process]$Proc)
    $hwnd = [IntPtr]::Zero
    for ($i = 0; $i -lt 60; $i++) {
        $Proc.Refresh()
        if ($Proc.HasExited) { throw "qaq-bx-ui.exe 提前退出 ($($Proc.ExitCode))" }
        if ($Proc.MainWindowHandle -ne [IntPtr]::Zero) { $hwnd = $Proc.MainWindowHandle; break }
        Start-Sleep -Milliseconds 250
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "等待主窗口超时" }
    [void][UiEdgeNative]::ShowWindow($hwnd, [UiEdgeNative]::SW_RESTORE)
    [void][UiEdgeNative]::SetForegroundWindow($hwnd)
    return $hwnd
}

# ---------- 场景 E1：访客态（无本地凭据） ----------
Write-Host "[E1] 访客态：临时移开 credentials.json" -ForegroundColor Cyan
$credMoved = $false
if (Test-Path $credPath) {
    Move-Item -Path $credPath -Destination $credBackup -Force
    $credMoved = $true
}
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 2500 # 首屏与访客 feed
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $winW = $rect.Right - $rect.Left; $winH = $rect.Bottom - $rect.Top

    Capture-WindowFrame -Hwnd $hwnd -FileName "e1-guest-home.png"
    # 访客态点击卡片进播放页（评论接口公开可读）
    Click-Fast -X ($winX + [int]($winW * 0.36)) -Y ($winY + 290)
    Start-Sleep -Milliseconds 2600
    Capture-WindowFrame -Hwnd $hwnd -FileName "e2-guest-player.png"
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($credMoved) {
        Move-Item -Path $credBackup -Destination $credPath -Force
        Write-Host "[E1] credentials.json 已恢复" -ForegroundColor Green
    }
}

# ---------- 场景 E3：快速连续切视频（播放代次取消路径） ----------
Write-Host "[E3] 快速连续切视频" -ForegroundColor Cyan
Start-Sleep -Milliseconds 800
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 2200
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $winW = $rect.Right - $rect.Left; $winH = $rect.Bottom - $rect.Top

    # 点击卡片 A → 200ms 后立刻返回（打断解析/头像/评论在飞请求）→ 立刻点击卡片 B
    Click-Fast -X ($winX + [int]($winW * 0.30)) -Y ($winY + 290)
    Start-Sleep -Milliseconds 200
    Click-Fast -X ($winX + 24) -Y ($winY + 24)   # 标题栏返回
    Start-Sleep -Milliseconds 250
    Click-Fast -X ($winX + [int]($winW * 0.62)) -Y ($winY + 290)
    Start-Sleep -Milliseconds 2800
    Capture-WindowFrame -Hwnd $hwnd -FileName "e3-fast-switch-player.png"

    # 立刻再切一次（连续代次递增）
    Click-Fast -X ($winX + 24) -Y ($winY + 24)
    Start-Sleep -Milliseconds 250
    Click-Fast -X ($winX + [int]($winW * 0.44)) -Y ($winY + 560)
    Start-Sleep -Milliseconds 2800
    Capture-WindowFrame -Hwnd $hwnd -FileName "e4-second-switch.png"

    # 切入后滚动评论区
    [void][UiEdgeNative]::SetCursorPos(($winX + [int]($winW * 0.35)), ($winY + [int]($winH * 0.55)))
    Scroll-Fast -TotalDelta -760
    Start-Sleep -Milliseconds 900
    Capture-WindowFrame -Hwnd $hwnd -FileName "e5-fast-switch-comments.png"
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}

# ---------- 场景 E6：热门页真实数据直启 + 点击首卡 ----------
Write-Host "[E6] 热门页（QAQ_BX_ROUTE=popular）真实 popular API 数据" -ForegroundColor Cyan
$env:QAQ_BX_ROUTE = "popular"
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 3000 # 等待真实分页数据与封面
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top

    Capture-WindowFrame -Hwnd $hwnd -FileName "e6-popular-real.png"
    # 首行卡片点击（网格首卡中心区域）→ 应与点首页卡同管线进播放页
    Click-Fast -X ($winX + 330) -Y ($winY + 360)
    Start-Sleep -Milliseconds 2800
    Capture-WindowFrame -Hwnd $hwnd -FileName "e7-popular-card-player.png"
    $proc.Refresh()
    if ($proc.HasExited) { throw "热门页点卡后进程退出 ($($proc.ExitCode))" }
} finally {
    Remove-Item Env:QAQ_BX_ROUTE -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}

# ---------- 场景 E7：播放页相关推荐真实数据 + 点击相关推荐卡 ----------
Write-Host "[E7] 相关推荐（QAQ_BX_VIDEO）真实 archive/related 数据" -ForegroundColor Cyan
$env:QAQ_BX_VIDEO = "BV1GJ411x7h7"
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 3200 # 等待相关推荐拉取与右栏封面
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $winW = $rect.Right - $rect.Left

    Capture-WindowFrame -Hwnd $hwnd -FileName "e8-player-related-real.png"
    # 右栏 360px 相关推荐首卡（标题区 y≈150-260）
    Click-Fast -X ($winX + $winW - 180) -Y ($winY + 210)
    Start-Sleep -Milliseconds 2800
    Capture-WindowFrame -Hwnd $hwnd -FileName "e9-related-card-opened.png"
    $proc.Refresh()
    if ($proc.HasExited) { throw "相关推荐点卡后进程退出 ($($proc.ExitCode))" }
} finally {
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}

# ---------- 场景 E8：Mobile 窄屏形态（最小宽高钳制 + 底栏 5-Tab + 2×3 更多 + 2 列开播） ----------
Write-Host "[E8] Mobile 窄屏形态走查（390x640 钳制与 5-Tab 交互）" -ForegroundColor Cyan
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 2500

    $dpi = [Math]::Max(96, [int][UiEdgeNative]::GetDpiForWindow($hwnd))
    $scale = $dpi / 96.0
    $wRect = New-Object UiEdgeNative+RECT
    $cRect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$wRect)
    [void][UiEdgeNative]::GetClientRect($hwnd, [ref]$cRect)
    $nonClientW = ($wRect.Right - $wRect.Left) - ($cRect.Right - $cRect.Left)
    $nonClientH = ($wRect.Bottom - $wRect.Top) - ($cRect.Bottom - $cRect.Top)

    # 1. 拖窄右边框超越 390px 下限，验证最小客户区宽度不小于 390 DIP
    Drag-WindowEdge -StartX ($wRect.Right - 2) -StartY ($wRect.Top + 260) -EndX ($wRect.Left + [int](200 * $scale)) -EndY ($wRect.Top + 260)
    [void][UiEdgeNative]::GetClientRect($hwnd, [ref]$cRect)
    $clientDipW = ($cRect.Right - $cRect.Left) / $scale
    if ($clientDipW -lt 388.0) {
        throw "最小客户区宽度钳制失效: 实际=${clientDipW} DIP < 390 DIP"
    }

    # 2. 统一调至 390×760 DIP 标准 Mobile 视口截图与点击
    $targetOuterW = [int][Math]::Round(390.0 * $scale) + $nonClientW
    $targetOuterH = [int][Math]::Round(760.0 * $scale) + $nonClientH
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $wRect.Left, $wRect.Top, $targetOuterW, $targetOuterH, [UiEdgeNative]::SWP_NOZORDER)
    Start-Sleep -Milliseconds 800
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$wRect)
    $mX = $wRect.Left; $mY = $wRect.Top
    $mW = $wRect.Right - $wRect.Left; $mH = $wRect.Bottom - $wRect.Top

    Capture-WindowFrame -Hwnd $hwnd -FileName "e10-mobile-home.png"

    # 底栏第 2 Tab「热门」 (col 1 center = 30% 宽, 距底 ~32 DIP)
    $barY = $mY + $mH - [int](32 * $scale)
    Click-Fast -X ($mX + [int]($mW * 0.30)) -Y $barY
    Start-Sleep -Milliseconds 2600
    Capture-WindowFrame -Hwnd $hwnd -FileName "e11-mobile-popular.png"

    # 底栏第 5 Tab「更多」 (col 4 center = 90% 宽)
    Click-Fast -X ($mX + [int]($mW * 0.90)) -Y $barY
    Start-Sleep -Milliseconds 450
    Capture-WindowFrame -Hwnd $hwnd -FileName "e12-mobile-more.png"

    # 点击「更多」2×3 菜单右下角「设置」并截图（右下偏上约 90 DIP）
    Click-Fast -X ($mX + $mW - [int](68 * $scale)) -Y ($mY + $mH - [int](96 * $scale))
    Start-Sleep -Milliseconds 500
    Capture-WindowFrame -Hwnd $hwnd -FileName "e13-mobile-settings.png"

    # 切回「推荐」(col 0 center = 10% 宽) 并点击左列首卡开播（吸顶播放页）
    Click-Fast -X ($mX + [int]($mW * 0.10)) -Y $barY
    Start-Sleep -Milliseconds 700
    Click-Fast -X ($mX + [int]($mW * 0.28)) -Y ($mY + [int](185 * $scale))
    Start-Sleep -Milliseconds 2600
    Capture-WindowFrame -Hwnd $hwnd -FileName "e14-mobile-player.png"

    $proc.Refresh()
    if ($proc.HasExited) { throw "Mobile 走查后进程退出 ($($proc.ExitCode))" }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}

# ---------- 场景 E9：全局内敛搜索框——真实键入 + Enter 直接发起搜索 ----------
Write-Host "[E9] 全局搜索框键入 rust 后按 Enter 发起搜索" -ForegroundColor Cyan
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    # 前台硬校验：GetForegroundWindow 必须是 qaq-bx-ui，否则 SendKeys 会泄漏到别的窗口
    # （IDE/输入法）。ALT 按下-释放模拟解锁前台锁后重试；仍失败则判场景 FAIL。
    $ok = $false
    for ($i = 0; $i -lt 10 -and -not $ok; $i++) {
        if ([UiEdgeNative]::GetForegroundWindow() -eq $hwnd) { $ok = $true; break }
        [UiEdgeNative]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        [UiEdgeNative]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        [void][UiEdgeNative]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 300
    }
    if (-not $ok) { throw "E9 FAIL: 无法把 qaq-bx-ui 置前台" }
    Start-Sleep -Milliseconds 1500
    $dpi = [Math]::Max(96, [int][UiEdgeNative]::GetDpiForWindow($hwnd))
    $scale = $dpi / 96.0
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $winW = $rect.Right - $rect.Left

    # 标题栏 Tall 内容行：全局搜索框居中偏左（约 48% 宽、24 DIP 高）
    Click-Fast -X ($winX + [int]($winW * 0.48)) -Y ($winY + [int](24 * $scale))
    Start-Sleep -Milliseconds 300
    # 剪贴板粘贴进框：绕开中文 IME——直键字母会进拼音组合态，
    # 且组合态的 {ENTER} 会被 IME 吃掉（提交候选词而非搜索）。
    Set-Clipboard -Value "rust"
    [System.Windows.Forms.SendKeys]::SendWait("^v")
    Start-Sleep -Milliseconds 400
    Capture-WindowFrame -Hwnd $hwnd -FileName "e15-search-draft.png"
    [System.Windows.Forms.SendKeys]::SendWait("{ENTER}")
    Start-Sleep -Milliseconds 4000 # 真实 WBI 搜索 + 结果封面
    Capture-WindowFrame -Hwnd $hwnd -FileName "e16-search-enter-results.png"
    $proc.Refresh()
    if ($proc.HasExited) { throw "搜索框 Enter 后进程退出 ($($proc.ExitCode))" }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}

Write-Host "[done] 极端场景走查完成，截图位于 $FramesDir" -ForegroundColor Green
