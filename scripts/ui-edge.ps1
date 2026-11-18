<#
.SYNOPSIS
    QAQ-Bx 极端场景走查：访客态（无本地凭据）与快速连续切视频。
.EXAMPLE
    .\scripts\ui-edge.ps1                   # 构建并跑全场景，截图输出 target/ui-demo/frames-edge/
    .\scripts\ui-edge.ps1 -SkipBuild        # 跳过构建
    .\scripts\ui-edge.ps1 -SkipBuild -Scenario E10
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string[]]$Scenario = @("all")
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$OutRoot = Join-Path $RepoRoot "target\ui-demo"
$FramesDir = Join-Path $OutRoot "frames-edge"
New-Item -ItemType Directory -Force -Path $FramesDir | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Runtime.InteropServices;

public static class UiEdgeNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
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
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdcBlt, uint nFlags);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP = 0x0004;
    public const uint MOUSEEVENTF_WHEEL = 0x0800;
    public const uint SWP_NOZORDER = 0x0004;
    public const int SW_RESTORE = 9;
}
"@
try { [void][UiEdgeNative]::SetProcessDpiAwarenessContext([IntPtr](-4)) } catch {}

function Capture-WindowFrame {
    param([IntPtr]$Hwnd, [string]$FileName, [string]$Mode = "print")
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($Hwnd, [ref]$rect)
    $w = [Math]::Max(1, $rect.Right - $rect.Left)
    $h = [Math]::Max(1, $rect.Bottom - $rect.Top)
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    if ($Mode -eq "screen") {
        # 屏幕直捕(MPE/DComp 视频层 PrintWindow 拍不到;需窗口已 TOPMOST)。
        $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
    } else {
        try {
            $hdc = $g.GetHdc()
            try {
                # PW_RENDERFULLCONTENT(2):WinUI3/DComp 窗口直捕,免 TOPMOST/前台竞争;
                # 失败再退 flag 0。
                if (-not [UiEdgeNative]::PrintWindow($Hwnd, $hdc, 2)) {
                    [void][UiEdgeNative]::PrintWindow($Hwnd, $hdc, 0)
                }
            } finally {
                $g.ReleaseHdc($hdc)
            }
        } catch {
            [void][UiEdgeNative]::GetWindowRect($Hwnd, [ref]$rect)
            try { $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h))) } catch {}
        }
    }
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

function Should-RunScenario {
    param([string]$Name)
    foreach ($item in $Scenario) {
        if ($item -ieq "all" -or $item -ieq $Name) { return $true }
    }
    return $false
}

function Find-ProcessMainWindow {
    param([int]$TargetPid)
    $script:edgeBestArea = 0
    $script:edgeFoundHwnd = [IntPtr]::Zero
    $cb = [UiEdgeNative+EnumWindowsProc]{
        param($hWnd, $lParam)
        $p = [uint32]0
        [void][UiEdgeNative]::GetWindowThreadProcessId($hWnd, [ref]$p)
        if ($p -eq $TargetPid -and [UiEdgeNative]::IsWindowVisible($hWnd)) {
            $r = New-Object UiEdgeNative+RECT
            if ([UiEdgeNative]::GetWindowRect($hWnd, [ref]$r)) {
                $w = $r.Right - $r.Left
                $h = $r.Bottom - $r.Top
                if ($w -ge 350 -and $h -ge 350 -and ($w * $h) -gt $script:edgeBestArea) {
                    $script:edgeBestArea = $w * $h
                    $script:edgeFoundHwnd = $hWnd
                }
            }
        }
        return $true
    }
    [void][UiEdgeNative]::EnumWindows($cb, [IntPtr]::Zero)
    return $script:edgeFoundHwnd
}

function Wait-MainWindow {
    param([System.Diagnostics.Process]$Proc)
    $hwnd = [IntPtr]::Zero
    for ($i = 0; $i -lt 60; $i++) {
        $Proc.Refresh()
        if ($Proc.HasExited) { throw "qaq-bx-ui.exe 提前退出 ($($Proc.ExitCode))" }
        $hwnd = Find-ProcessMainWindow -TargetPid $Proc.Id
        if ($hwnd -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "等待主窗口超时" }
    [void][UiEdgeNative]::ShowWindow($hwnd, [UiEdgeNative]::SW_RESTORE)
    [void][UiEdgeNative]::SetForegroundWindow($hwnd)
    return $hwnd
}

# ---------- 场景 E1：访客态（无本地凭据） ----------
if (Should-RunScenario "E1") {
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
}

# ---------- 场景 E3：快速连续切视频（播放代次取消路径） ----------
if (Should-RunScenario "E3") {
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
}

# ---------- 场景 E6：热门页真实数据直启 + 点击首卡 ----------
if (Should-RunScenario "E6") {
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
}

# ---------- 场景 E7：播放页相关推荐真实数据 + 点击相关推荐卡 ----------
if (Should-RunScenario "E7") {
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
}

# ---------- 场景 E8：Mobile 窄屏形态（最小宽高钳制 + 底栏 5-Tab + 2×3 更多 + 2 列开播） ----------
if (Should-RunScenario "E8") {
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
}

# ---------- 场景 E9：全局内敛搜索框——真实键入 + Enter 直接发起搜索 ----------
if (Should-RunScenario "E9") {
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
}

# ---------- 场景 E11：快速伸缩窗口 → 视频区黑屏排查（截图 + 像素检测 + stderr 采集） ----------
if (Should-RunScenario "E11") {
Write-Host "[E11] 快速伸缩窗口排查视频黑屏（QAQ_BX_VIDEO 直开播放页）" -ForegroundColor Cyan
$env:QAQ_BX_VIDEO = "BV1GJ411x7h7"
$stderrLog = Join-Path $OutRoot "e11-stderr.log"
$proc = $null
try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_VIDEO"] = "BV1GJ411x7h7"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 4500 # 等起播出画

    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $baseW = $rect.Right - $rect.Left; $baseH = $rect.Bottom - $rect.Top
    $baseX = $rect.Left; $baseY = $rect.Top
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010) # TOPMOST | NOSIZE | NOMOVE
    Start-Sleep -Milliseconds 400
    Capture-WindowFrame -Hwnd $hwnd -FileName "e11-0-playing.png"

    # 快速交替伸缩：模拟用户猛拽窗口（大↔小,每步 ~55ms）
    Write-Host "[E11] rapid resize loop..." -ForegroundColor Cyan
    for ($i = 1; $i -le 30; $i++) {
        if ($i % 2 -eq 1) {
            $w = [int]($baseW * 0.55); $h = [int]($baseH * 0.55)
        } else {
            $w = $baseW; $h = $baseH
        }
        [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $baseX, $baseY, $w, $h, [UiEdgeNative]::SWP_NOZORDER)
        Start-Sleep -Milliseconds 55
    }
    # 回到基准尺寸后等引擎合并窗口/重挂全部走完
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Milliseconds 800
    Capture-WindowFrame -Hwnd $hwnd -FileName "e11-1-after-resize-800ms.png"
    Start-Sleep -Milliseconds 2500
    Capture-WindowFrame -Hwnd $hwnd -FileName "e11-2-after-resize-3300ms.png"
    Start-Sleep -Milliseconds 3000
    Capture-WindowFrame -Hwnd $hwnd -FileName "e11-3-after-resize-6300ms.png"
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-2), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010) # NOTOPMOST

    # 视频区黑屏像素检测（视频面约占窗口左上 8%-55% 宽 / 6%-48% 高）
    function Test-RegionBlack {
        param([string]$File, [double]$X0, [double]$Y0, [double]$X1, [double]$Y1)
        $bmp = New-Object System.Drawing.Bitmap($File)
        $x0 = [int]($bmp.Width * $X0); $y0 = [int]($bmp.Height * $Y0)
        $x1 = [int]($bmp.Width * $X1); $y1 = [int]($bmp.Height * $Y1)
        $sum = 0.0; $n = 0
        for ($y = $y0; $y -lt $y1; $y += 4) {
            for ($x = $x0; $x -lt $x1; $x += 4) {
                $p = $bmp.GetPixel($x, $y)
                $sum += ($p.R + $p.G + $p.B) / 3.0; $n++
            }
        }
        $bmp.Dispose()
        return [Math]::Round($sum / [Math]::Max(1, $n), 1)
    }
    foreach ($f in @("e11-1-after-resize-800ms.png","e11-2-after-resize-3300ms.png","e11-3-after-resize-6300ms.png")) {
        $lum = Test-RegionBlack -File (Join-Path $FramesDir $f) -X0 0.10 -Y0 0.10 -X1 0.50 -Y1 0.45
        Write-Host "[E11] $f video-region mean luminance = $lum" -ForegroundColor $(if ($lum -lt 12) { "Red" } else { "Green" })
    }

    $proc.Refresh()
    if ($proc.HasExited) { throw "E11: 进程提前退出 ($($proc.ExitCode))" }
} finally {
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask.Wait(3000)) { $stderrTask.Result | Set-Content -Path $stderrLog -Encoding UTF8; Write-Host "[E11] stderr → $stderrLog" -ForegroundColor DarkCyan }
    } catch {}
}
}

# ---------- 场景 E12：系统媒体引擎（QAQ_BX_NATIVE=1）播放 + 快速伸缩免疫验证 ----------
if (Should-RunScenario "E12") {
Write-Host "[E12] 系统媒体引擎直开播放页 + 快速伸缩验证（QAQ_BX_NATIVE=1）" -ForegroundColor Cyan
$global:E12Fail = $false
$stderrLog = Join-Path $OutRoot "e12-stderr.log"
$proc = $null
try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_VIDEO"] = "BV1GJ411x7h7"
    $psi.EnvironmentVariables["QAQ_BX_NATIVE"] = "1"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 6000 # 系统引擎起播（DASH 拉流 + 解码预热）

    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Milliseconds 400
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $baseW = $rect.Right - $rect.Left; $baseH = $rect.Bottom - $rect.Top
    $baseX = $rect.Left; $baseY = $rect.Top
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e12-0-native-playing.png"

    Write-Host "[E12] rapid resize loop..." -ForegroundColor Cyan
    for ($i = 1; $i -le 30; $i++) {
        if ($i % 2 -eq 1) {
            $w = [int]($baseW * 0.55); $h = [int]($baseH * 0.55)
        } else {
            $w = $baseW; $h = $baseH
        }
        [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $baseX, $baseY, $w, $h, [UiEdgeNative]::SWP_NOZORDER)
        Start-Sleep -Milliseconds 55
    }
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Milliseconds 800
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e12-1-after-resize-800ms.png"
    Start-Sleep -Milliseconds 2500
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e12-2-after-resize-3300ms.png"

    function Test-RegionBlackNative {
        param([string]$File, [double]$X0, [double]$Y0, [double]$X1, [double]$Y1)
        $bmp = New-Object System.Drawing.Bitmap($File)
        $x0 = [int]($bmp.Width * $X0); $y0 = [int]($bmp.Height * $Y0)
        $x1 = [int]($bmp.Width * $X1); $y1 = [int]($bmp.Height * $Y1)
        $sum = 0.0; $n = 0
        for ($y = $y0; $y -lt $y1; $y += 4) {
            for ($x = $x0; $x -lt $x1; $x += 4) {
                $p = $bmp.GetPixel($x, $y)
                $sum += ($p.R + $p.G + $p.B) / 3.0; $n++
            }
        }
        $bmp.Dispose()
        return [Math]::Round($sum / [Math]::Max(1, $n), 1)
    }
    foreach ($f in @("e12-0-native-playing.png","e12-1-after-resize-800ms.png","e12-2-after-resize-3300ms.png")) {
        $lum = Test-RegionBlackNative -File (Join-Path $FramesDir $f) -X0 0.10 -Y0 0.10 -X1 0.50 -Y1 0.45
        Write-Host "[E12] $f video-region mean luminance = $lum" -ForegroundColor $(if ($lum -lt 12) { "Red" } else { "Green" })
    }

    $proc.Refresh()
    if ($proc.HasExited) { throw "E12: 进程提前退出 ($($proc.ExitCode))" }
    # 心跳断言暂存标记(stderr 判定在 finally 里统一做)
    $global:E12ScreenshotsOk = $true
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask.Wait(3000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E12] stderr → $stderrLog" -ForegroundColor DarkCyan
            if ($text -match "native engine (init|load) failed|native engine attach failed") {
                Write-Host "[E12] FAIL: native 引擎装载/接管失败(见 stderr)" -ForegroundColor Red
                $global:E12Fail = $true
            } elseif ($text -notmatch "native attach") {
                Write-Host "[E12] FAIL: 未见 MPE player 接管锚点(attach 链未走通)" -ForegroundColor Red
                $global:E12Fail = $true
            } else {
                Write-Host "[E12] PASS: attach 接管成功且无装载失败" -ForegroundColor Green
            }
        }
    } catch {}
}
if ($global:E12Fail) { throw "E12 FAIL: native 引擎装载失败(详见 $stderrLog)" }
}

function Get-WindowGeometry {
    param([IntPtr]$Hwnd)
    $wr = New-Object UiEdgeNative+RECT
    $cr = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($Hwnd, [ref]$wr)
    [void][UiEdgeNative]::GetClientRect($Hwnd, [ref]$cr)
    return @{
        X = $wr.Left
        Y = $wr.Top
        W = $wr.Right - $wr.Left
        H = $wr.Bottom - $wr.Top
        CW = $cr.Right - $cr.Left
        CH = $cr.Bottom - $cr.Top
    }
}

function Find-PlayerFullscreenButton {
    param([System.Windows.Automation.AutomationElement]$Root)
    $condBtn = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button
    )
    $btns = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condBtn)
    for ($i = 0; $i -lt $btns.Count; $i++) {
        if ($btns[$i].Current.Name -eq "右下角小窗" -and ($i + 1) -lt $btns.Count) {
            return $btns[$i + 1]
        }
    }
    return $null
}

function Find-HudExitButton {
    param([System.Windows.Automation.AutomationElement]$Root)
    $condText = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty,
        "退出全屏"
    )
    $exitTxt = $Root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condText)
    if (-not $exitTxt) { return $null }
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    $cur = $exitTxt
    while ($cur -and $cur -ne $Root) {
        if ($cur.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button) {
            return $cur
        }
        $cur = $walker.GetParent($cur)
    }
    return $null
}

function Assert-FullscreenBounds {
    param([IntPtr]$Hwnd, [string]$StepName)
    $bounds = [System.Windows.Forms.Screen]::FromHandle($Hwnd).Bounds
    $g = Get-WindowGeometry -Hwnd $Hwnd
    if ($g.X -ne $bounds.X -or $g.Y -ne $bounds.Y -or $g.W -ne $bounds.Width -or $g.H -ne $bounds.Height -or $g.CW -ne $bounds.Width -or $g.CH -ne $bounds.Height) {
        throw "E10 FAIL ($StepName): 窗口未贴合屏幕无边框全屏 bounds=($($bounds.X),$($bounds.Y) $($bounds.Width)x$($bounds.Height)) actual=($($g.X),$($g.Y) outer=$($g.W)x$($g.H) client=$($g.CW)x$($g.CH))"
    }
}

# ---------- 场景 E10：全屏播放（按钮进入 / 退出恢复 / HUD 自隐 / 快捷键与双击） ----------
if (Should-RunScenario "E10") {
Write-Host "[E10] 全屏播放与退出走查（点击按钮 + 恢复尺寸 + HUD 自动隐藏 + 快捷键/双击）" -ForegroundColor Cyan
$env:QAQ_BX_VIDEO = "BV1GJ411x7h7"
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    $interactive = $false
    for ($i = 0; $i -lt 5 -and -not $interactive; $i++) {
        if ([UiEdgeNative]::GetForegroundWindow() -eq $hwnd) { $interactive = $true; break }
        [UiEdgeNative]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        [UiEdgeNative]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        [void][UiEdgeNative]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 250
    }
    if (-not $interactive) {
        Write-Host "[E10] 注意：桌面不可抢焦点（可能已锁屏），将使用 UIAutomation InvokePattern 完成全屏点击校验" -ForegroundColor Yellow
    }
    Start-Sleep -Milliseconds 4200 # 等待媒体解析与首帧呈现

    $orig = Get-WindowGeometry -Hwnd $hwnd
    Capture-WindowFrame -Hwnd $hwnd -FileName "e17-before-fullscreen.png"

    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $fsBtn = Find-PlayerFullscreenButton -Root $root
    if (-not $fsBtn) { throw "E10 FAIL: 播放页未定位到全屏按钮" }

    # 1. 点击播放页桌面播控条全屏按钮进入无边框全屏，并在 HUD 自隐前捕获 e18 与退出按钮
    $fsBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 500
    Assert-FullscreenBounds -Hwnd $hwnd -StepName "进入全屏"
    $exitBtn = Find-HudExitButton -Root $root
    if (-not $exitBtn) { throw "E10 FAIL: 进入全屏后顶部 HUD 未展示 [退出全屏] 按钮" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e18-fullscreen-hud.png"

    # 2. 退出全屏并校验窗口/客户区尺寸恢复（交互桌面测 Esc 键，锁屏桌面测 UIA 点击“退出全屏”）
    if ($interactive) {
        [UiEdgeNative]::keybd_event(0x1B, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 40
        [UiEdgeNative]::keybd_event(0x1B, 0, 2, [UIntPtr]::Zero)
    } else {
        $exitBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    }
    Start-Sleep -Milliseconds 800
    $rest = Get-WindowGeometry -Hwnd $hwnd
    if ([Math]::Abs($rest.W - $orig.W) -gt 24 -or [Math]::Abs($rest.H - $orig.H) -gt 24 -or [Math]::Abs($rest.CW - $orig.CW) -gt 24) {
        throw "E10 FAIL: 退出全屏后窗口尺寸未恢复 (期望 outer=$($orig.W)x$($orig.H) client=$($orig.CW)x$($orig.CH), 实际 outer=$($rest.W)x$($rest.H) client=$($rest.CW)x$($rest.CH))"
    }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e20-after-fullscreen-restore.png"

    # 3. 再次进入全屏（交互桌面测双击画面；锁屏桌面测 UIA 点击），静置 >3.6s 验证 HUD 自动隐藏
    if ($interactive) {
        $dpi = [Math]::Max(96, [int][UiEdgeNative]::GetDpiForWindow($hwnd))
        $scale = $dpi / 96.0
        $videoX = $rest.X + [int](380.0 * $scale)
        $videoY = $rest.Y + [int](260.0 * $scale)
        Click-Fast -X $videoX -Y $videoY
        Start-Sleep -Milliseconds 80
        Click-Fast -X $videoX -Y $videoY
    } else {
        $fsBtn2 = Find-PlayerFullscreenButton -Root $root
        if (-not $fsBtn2) { throw "E10 FAIL: 退出全屏恢复后播放页全屏按钮缺失" }
        $fsBtn2.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    }
    Start-Sleep -Milliseconds 700
    Assert-FullscreenBounds -Hwnd $hwnd -StepName "二次进入全屏"

    Start-Sleep -Milliseconds 3600
    $condBtn = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button
    )
    $btnsHidden = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condBtn)
    $visBtns = 0
    foreach ($b in $btnsHidden) {
        $r = $b.Current.BoundingRectangle
        if (-not ([double]::IsInfinity($r.X) -or [double]::IsNaN($r.X))) { $visBtns++ }
    }
    if ($visBtns -ne 0) {
        throw "E10 FAIL: 全屏静置后 HUD 未自动隐藏 (可见按钮数=$visBtns)"
    }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e19-fullscreen-autohide.png"

    # 4. 交互桌面下追加验证 F 键退出全屏恢复
    if ($interactive) {
        [UiEdgeNative]::keybd_event(0x46, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 40
        [UiEdgeNative]::keybd_event(0x46, 0, 2, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 800
        $rest2 = Get-WindowGeometry -Hwnd $hwnd
        if ([Math]::Abs($rest2.W - $orig.W) -gt 24 -or [Math]::Abs($rest2.H - $orig.H) -gt 24) {
            throw "E10 FAIL: F 键退出全屏后窗口尺寸未恢复 ($($rest2.W)x$($rest2.H) vs $($orig.W)x$($orig.H))"
        }
    }

    $proc.Refresh()
    if ($proc.HasExited) { throw "全屏走查后进程意外退出 ($($proc.ExitCode))" }
} finally {
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}
}

Write-Host "[done] 极端场景走查完成，截图位于 $FramesDir" -ForegroundColor Green
