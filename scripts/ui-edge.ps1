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

function Grab-WindowBitmap {
    param([IntPtr]$Hwnd, [string]$Mode = "print")
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($Hwnd, [ref]$rect)
    $w = [Math]::Max(1, $rect.Right - $rect.Left)
    $h = [Math]::Max(1, $rect.Bottom - $rect.Top)
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    if ($Mode -eq "screen") {
        # 屏幕直捕(需窗口已 TOPMOST)。
        $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
    } else {
        try {
            $hdc = $g.GetHdc()
            try {
                # PW_RENDERFULLCONTENT(2):WinUI3/DComp 窗口直捕,免 TOPMOST/前台竞争;失败退 flag 0。
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
    return $bmp
}

function Capture-WindowFrame {
    param([IntPtr]$Hwnd, [string]$FileName, [string]$Mode = "print")
    $bmp = Grab-WindowBitmap -Hwnd $Hwnd -Mode $Mode
    $path = Join-Path $FramesDir $FileName
    try {
        $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $bmp.Dispose()
    }
    Write-Host "[shot] $path" -ForegroundColor DarkCyan
}

function Measure-ImageRegionStats {
    param([string]$File, [double]$X0, [double]$Y0, [double]$X1, [double]$Y1)
    $bmp = New-Object System.Drawing.Bitmap($File)
    try {
        $x0 = [Math]::Min([Math]::Max([int]($bmp.Width * $X0), 0), [Math]::Max(0, $bmp.Width - 1))
        $y0 = [Math]::Min([Math]::Max([int]($bmp.Height * $Y0), 0), [Math]::Max(0, $bmp.Height - 1))
        $x1 = [Math]::Min([Math]::Max([int]($bmp.Width * $X1), $x0 + 1), $bmp.Width)
        $y1 = [Math]::Min([Math]::Max([int]($bmp.Height * $Y1), $y0 + 1), $bmp.Height)
        $sum = 0.0
        $sumSq = 0.0
        $n = 0
        for ($y = $y0; $y -lt $y1; $y += 12) {
            for ($x = $x0; $x -lt $x1; $x += 12) {
                $p = $bmp.GetPixel($x, $y)
                $v = ($p.R + $p.G + $p.B) / 3.0
                $sum += $v
                $sumSq += ($v * $v)
                $n++
            }
        }
        $cnt = [Math]::Max(1, $n)
        $mean = $sum / $cnt
        $variance = [Math]::Max(0.0, ($sumSq / $cnt) - ($mean * $mean))
        return @{
            Mean   = [Math]::Round($mean, 1)
            StdDev = [Math]::Round([Math]::Sqrt($variance), 1)
        }
    } finally {
        $bmp.Dispose()
    }
}

function Measure-ImageRegionLuma {
    param([string]$File, [double]$X0, [double]$Y0, [double]$X1, [double]$Y1)
    return (Measure-ImageRegionStats -File $File -X0 $X0 -Y0 $Y0 -X1 $X1 -Y1 $Y1).Mean
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

function Move-Cursor {
    # SetCursorPos 只挪指针不产生 WM_MOUSEMOVE,hover/PointerEntered 需真实输入消息:
    # 走 mouse_event MOVE|ABSOLUTE(坐标归一化到主屏 0..65535)。
    param([int]$X, [int]$Y)
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $nx = [int](($X * 65535) / [Math]::Max(1, $bounds.Width - 1))
    $ny = [int](($Y * 65535) / [Math]::Max(1, $bounds.Height - 1))
    [void][UiEdgeNative]::mouse_event(0x0001 -bor 0x8000, $nx, $ny, 0, [UIntPtr]::Zero)
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
    for ($i = 0; $i -lt 120; $i++) {
        $Proc.Refresh()
        if ($Proc.HasExited) { throw "qaq-bx-ui.exe 提前退出 ($($Proc.ExitCode))" }
        $hwnd = Find-ProcessMainWindow -TargetPid $Proc.Id
        # EnumWindows 偶发漏窗(冷启动/杀软扫描新 exe 拖慢首帧):.NET 句柄兜底,
        # 但必须验尺寸 —— .NET 可能拿到 1×1 的 NonClient Sink 占位窗。
        if ($hwnd -eq [IntPtr]::Zero -and $Proc.MainWindowHandle -ne [IntPtr]::Zero) {
            $r = New-Object UiEdgeNative+RECT
            if ([UiEdgeNative]::GetWindowRect($Proc.MainWindowHandle, [ref]$r)) {
                $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
                if ($w -ge 350 -and $h -ge 350) { $hwnd = $Proc.MainWindowHandle }
            }
        }
        if ($hwnd -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 250
    }
    if ($hwnd -eq [IntPtr]::Zero) {
        # 诊断:列出该 pid 的全部可见窗口与 .NET 句柄,别让超时吞掉现场。
        Write-Host "[wait] 诊断 pid=$($Proc.Id) net=$($Proc.MainWindowHandle)"
        $cb = [UiEdgeNative+EnumWindowsProc]{
            param($hWnd, $lParam)
            $p = [uint32]0
            [void][UiEdgeNative]::GetWindowThreadProcessId($hWnd, [ref]$p)
            if ($p -eq $Proc.Id) {
                $r2 = New-Object UiEdgeNative+RECT
                [void][UiEdgeNative]::GetWindowRect($hWnd, [ref]$r2)
                Write-Host "[wait]   hwnd=$hWnd vis=$([UiEdgeNative]::IsWindowVisible($hWnd)) rect=$($r2.Left),$($r2.Top),$($r2.Right-$r2.Left)x$($r2.Bottom-$r2.Top)"
            }
            return $true
        }
        [void][UiEdgeNative]::EnumWindows($cb, [IntPtr]::Zero)
        throw "等待主窗口超时"
    }
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
    # M7:播放引擎已产品化为设置项;E11 走 mpv 通路必须显式钉死(env 覆盖优先于
    # settings.json，否则上一次走查留下的 native 会让本场景静默改道)。
    $psi.EnvironmentVariables["QAQ_BX_NATIVE"] = "0"
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
        for ($y = $y0; $y -lt $y1; $y += 12) {
            for ($x = $x0; $x -lt $x1; $x += 12) {
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
    Capture-WindowFrame -Hwnd $hwnd -FileName "e12-0-native-playing.png"

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
    Capture-WindowFrame -Hwnd $hwnd -FileName "e12-1-after-resize-800ms.png"
    Start-Sleep -Milliseconds 2500
    Capture-WindowFrame -Hwnd $hwnd -FileName "e12-2-after-resize-3300ms.png"
    Start-Sleep -Milliseconds 3000
    Capture-WindowFrame -Hwnd $hwnd -FileName "e12-3-after-resize-6300ms.png"

    $e12PostStats = @{ Mean = 0.0; StdDev = 0.0 }
    foreach ($f in @("e12-0-native-playing.png","e12-1-after-resize-800ms.png","e12-2-after-resize-3300ms.png","e12-3-after-resize-6300ms.png")) {
        $st = Measure-ImageRegionStats -File (Join-Path $FramesDir $f) -X0 0.10 -Y0 0.10 -X1 0.50 -Y1 0.45
        if ($f -eq "e12-3-after-resize-6300ms.png") { $e12PostStats = $st }
        Write-Host "[E12] $f video-region mean=$($st.Mean) stddev=$($st.StdDev)" -ForegroundColor $(if ($st.Mean -le 15 -or $st.StdDev -lt 6) { "Red" } else { "Green" })
    }
    if ($e12PostStats.Mean -le 15 -or $e12PostStats.StdDev -lt 6) {
        Write-Host "[E12] FAIL: 快速缩放稳定后画面异常/纯色 (mean=$($e12PostStats.Mean) stddev=$($e12PostStats.StdDev))" -ForegroundColor Red
        $global:E12Fail = $true
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
            if ($text -match "native engine (init|load) failed|native engine attach failed|native build_source failed|native media failed|danmaku stage: (device|swapchain|begin_draw|present|ResizeBuffers)") {
                Write-Host "[E12] FAIL: native 引擎装载/接管/弹幕透明层出错(见 stderr)" -ForegroundColor Red
                $global:E12Fail = $true
            } elseif ($text -notmatch "native build_source ok") {
                Write-Host "[E12] FAIL: 未见装载线程产出锚点(build_source 未在后台线程完成)" -ForegroundColor Red
                $global:E12Fail = $true
            } elseif ($text -notmatch "native attach") {
                Write-Host "[E12] FAIL: 未见 MPE player 接管锚点(attach 链未走通)" -ForegroundColor Red
                $global:E12Fail = $true
            } elseif ($text -notmatch "native media opened") {
                Write-Host "[E12] FAIL: 系统媒体管线未开画(MediaOpened 未到)" -ForegroundColor Red
                $global:E12Fail = $true
            } elseif ($text -notmatch "STAGE-A: danmaku surface ok") {
                Write-Host "[E12] FAIL: 弹幕透明层未建立(层线程未产出 swapchain)" -ForegroundColor Red
                $global:E12Fail = $true
            } elseif ($text -match "native surface (detached|remount)" -and $text -notmatch "native surface remount") {
                # 面板重建必须走「换新源」路径;只拆不建 = 旧源被作废 player 绑走后失联
                Write-Host "[E12] FAIL: 面板拆走后未看到换源续播(重建链断在 detach 侧)" -ForegroundColor Red
                $global:E12Fail = $true
            } else {
                Write-Host "[E12] PASS: 后台装载 + attach 接管 + 管线开画 + 弹幕透明层就位" -ForegroundColor Green
            }
        }
    } catch {}
}
if ($global:E12Fail) { throw "E12 FAIL: native 引擎装载失败(详见 $stderrLog)" }
}

# ---------- 场景 E13：系统媒体引擎的 SMTC(系统媒体控件)与弹幕绘制链验证 ----------
# 不做窗口伸缩,保证 stderr 里「Playing → NotPlaying」的状态翻转只能来自系统媒体键,
# 即锁屏/控制栏的播放暂停按钮真的作用到了我们的播放器上(宪法 #4 的模块级自动化)。
if (Should-RunScenario "E13") {
Write-Host "[E13] native SMTC + 弹幕绘制链验证（QAQ_BX_NATIVE=1，静置不起伸缩）" -ForegroundColor Cyan
$global:E13Fail = $false
$stderrLog = Join-Path $OutRoot "e13-stderr.log"
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
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Seconds 10   # 装载 + 开画 + 弹幕分段到货
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e13-0-native-playing.png"

    Write-Host "[E13] press system media play/pause key" -ForegroundColor Cyan
    [UiEdgeNative]::keybd_event(0xB3, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [UiEdgeNative]::keybd_event(0xB3, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Seconds 4
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e13-1-after-media-key.png"

    # 再按一次恢复播放(状态应回到 Playing;顺带验 resume 语义没把播放意愿弄丢)
    [UiEdgeNative]::keybd_event(0xB3, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 40
    [UiEdgeNative]::keybd_event(0xB3, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Seconds 4

    $proc.Refresh()
    if ($proc.HasExited) { throw "E13: 进程提前退出 ($($proc.ExitCode))" }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask.Wait(3000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E13] stderr → $stderrLog" -ForegroundColor DarkCyan
            # 「第一次 Playing 之后」是否出现 NotPlaying:即系统媒体键真的把播放
            # 按停了(键由 MediaPlayer 自持 CommandManager 直接消化,不需要我们转发)。
            $playSegs = $text -split "smtc native: status=Playing"
            $keyPaused = ($playSegs.Count -ge 2) -and ($playSegs[1] -match "status=NotPlaying")
            $keyResumed = $false
            if ($keyPaused) {
                $keyResumed = ($playSegs[1] -match "status=NotPlaying[\s\S]*status=Playing")
            }
            if ($text -match "native media failed|native engine (init|load) failed|native engine attach failed") {
                Write-Host "[E13] FAIL: native 播放链报错(见 stderr)" -ForegroundColor Red
                $global:E13Fail = $true
            } elseif ($text -notmatch "smtc native: 控件已绑定") {
                Write-Host "[E13] FAIL: 未见 MediaPlayer 自持 SMTC 绑定锚点" -ForegroundColor Red
                $global:E13Fail = $true
            } elseif ($text -notmatch "smtc native: status=Playing") {
                Write-Host "[E13] FAIL: 系统媒体控件未进入 Playing(锁屏/控制栏不会显示在播)" -ForegroundColor Red
                $global:E13Fail = $true
            } elseif ($text -notmatch "STAGE-B: danmaku 首帧已绘制") {
                Write-Host "[E13] FAIL: 弹幕透明层没有画出首帧(数据链或绘制链断了)" -ForegroundColor Red
                $global:E13Fail = $true
            } elseif (-not $keyPaused) {
                Write-Host "[E13] FAIL: 系统媒体键没能暂停播放(SMTC 指令通道未作用到 player)" -ForegroundColor Red
                $global:E13Fail = $true
            } else {
                Write-Host "[E13] PASS: SMTC 绑定/在播状态/弹幕首帧/系统媒体键全部到位(二次按键恢复播放=$keyResumed)" -ForegroundColor Green
            }
        }
    } catch {}
}
if ($global:E13Fail) { throw "E13 FAIL: native SMTC/弹幕绘制链验证失败(详见 $stderrLog)" }
}

function Invoke-UiaElement {
    # 返回按钮一类控件在锁屏桌面可能报 ElementNotEnabledException；退到坐标点击。
    param([System.Windows.Automation.AutomationElement]$Element, [string]$What)
    try {
        $Element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        return $true
    } catch {
        Write-Host "[uia] $What InvokePattern 失败($($_.Exception.GetType().Name))，退坐标点击" -ForegroundColor Yellow
    }
    $r = $Element.Current.BoundingRectangle
    if ($r.Width -le 0 -or $r.Height -le 0) { return $false }
    Click-Fast -X ([int]($r.X + $r.Width / 2)) -Y ([int]($r.Y + $r.Height / 2))
    return $true
}

function Find-UiaByAutomationId {
    param([System.Windows.Automation.AutomationElement]$Root, [string]$Id)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id
    )
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Find-UiaByName {
    param([System.Windows.Automation.AutomationElement]$Root, [string]$Name)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name
    )
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Select-UiaNav {
    # 导航项按 ListItem + Name 定位（页面标题也是同名 Text，必须排除）。
    param([System.Windows.Automation.AutomationElement]$Root, [string]$Name)
    $condName = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name
    )
    $condType = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::ListItem
    )
    $cond = New-Object System.Windows.Automation.AndCondition($condName, $condType)
    $item = $Root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    if (-not $item) { Write-Host "[uia] 导航项 '$Name' 缺失" -ForegroundColor Red; return $false }
    $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    return $true
}

function Select-UiaEngineItem {
    # 设置页「播放引擎」下拉：WinUI 的下拉项只有 Expand 之后才进 UIA 树。
    param([System.Windows.Automation.AutomationElement]$Root, [string]$ItemPattern)
    $combo = Find-UiaByAutomationId -Root $Root -Id "settings-player-engine"
    if (-not $combo) { Write-Host "[uia] 未找到 settings-player-engine 下拉框" -ForegroundColor Red; return $false }
    try {
        $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    } catch { Write-Host "[uia] 展开播放引擎下拉失败: $($_.Exception.Message)" -ForegroundColor Red; return $false }
    Start-Sleep -Milliseconds 800
    $condList = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::ListItem
    )
    $items = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condList)
    $pick = $null
    for ($i = 0; $i -lt $items.Count; $i++) {
        if ($items[$i].Current.Name -match $ItemPattern) { $pick = $items[$i] }
    }
    if (-not $pick) { Write-Host "[uia] 播放引擎下拉内未找到 '$ItemPattern' 项" -ForegroundColor Red; return $false }
    Write-Host "[uia] 选中播放引擎项: '$($pick.Current.Name)'" -ForegroundColor DarkCyan
    $pick.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    return $true
}

# ---------- 场景 E14：拖框风暴 UI 线程量化（RESIZE-STORM 计数器 + CPU 峰值 + 亮度回归） ----------
# 量化拖框期 UI 线程节拍：快速伸缩(30×55ms,复用 E11 节奏)+ 慢速 ±40px 往返(模拟人手拖框)。
# app 侧在 WindowResized 入口按连续间隔归并风暴并输出 `RESIZE-STORM n=… span_ms=… max_gap_ms=…`；
# n = 风暴期实际消费的消息数,F1 合并生效度 = 产出步数(30/24) − n。§5 验收 2/5 的数据源。
if (Should-RunScenario "E14") {
Write-Host "[E14] 拖框风暴量化（QAQ_BX_VIDEO 直开播放页：快速伸缩 + 慢速拖框,CPU/计数器采样）" -ForegroundColor Cyan
$global:E14Fail = $false
$stderrLog = Join-Path $OutRoot "e14-stderr.log"
$proc = $null
$sink = $null; $task = $null
try {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_VIDEO"] = "BV1GJ411x7h7"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $sink = $proc.StandardError
    $task = $sink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 4500 # 起播出画
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Milliseconds 400
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $baseW = $rect.Right - $rect.Left; $baseH = $rect.Bottom - $rect.Top
    $baseX = $rect.Left; $baseY = $rect.Top
    $cores = [Math]::Max(1, [int]$env:NUMBER_OF_PROCESSORS)

    # 相位 A：快速伸缩 30×55ms(55%↔100%)。
    Write-Host "[E14] phase A: rapid resize 30x55ms" -ForegroundColor Cyan
    $cpu0 = $proc.TotalProcessorTime.TotalMilliseconds
    $wall0 = [DateTime]::UtcNow
    for ($i = 1; $i -le 30; $i++) {
        if ($i % 2 -eq 1) { $w = [int]($baseW * 0.55); $h = [int]($baseH * 0.55) }
        else { $w = $baseW; $h = $baseH }
        [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $baseX, $baseY, $w, $h, [UiEdgeNative]::SWP_NOZORDER)
        Start-Sleep -Milliseconds 55
    }
    Start-Sleep -Milliseconds 500 # settled(150ms) 收口相位 A 风暴
    $cpu1 = $proc.TotalProcessorTime.TotalMilliseconds
    $wall1 = [DateTime]::UtcNow

    # 相位 B：慢速 ±40px 往返 24×30ms(模拟人手拖框)。
    Write-Host "[E14] phase B: slow drag 24x30ms ±40px" -ForegroundColor Cyan
    for ($i = 1; $i -le 24; $i++) {
        $offset = if ($i % 2 -eq 1) { 40 } else { 0 }
        [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $baseX, $baseY, $baseW + $offset, $baseH, [UiEdgeNative]::SWP_NOZORDER)
        Start-Sleep -Milliseconds 30
    }
    $cpu2 = $proc.TotalProcessorTime.TotalMilliseconds
    $wall2 = [DateTime]::UtcNow
    Start-Sleep -Milliseconds 900 # settled + 画面回稳

    Capture-WindowFrame -Hwnd $hwnd -FileName "e14-0-after-storm.png"
    Start-Sleep -Milliseconds 2000
    Capture-WindowFrame -Hwnd $hwnd -FileName "e14-1-after-storm-2s.png"

    # 进程 CPU 峰值(全核折算):分相位记录,数据回填研究文档 §6。
    $cpuPctA = [Math]::Round((($cpu1 - $cpu0) / ((($wall1 - $wall0).TotalMilliseconds) * $cores)) * 100, 1)
    $cpuPctB = [Math]::Round((($cpu2 - $cpu1) / ((($wall2 - $wall1).TotalMilliseconds) * $cores)) * 100, 1)
    Write-Host "[E14] process CPU: phase A=$cpuPctA% phase B=$cpuPctB% (logical cores=$cores)" -ForegroundColor Cyan

    function Test-RegionBlackE14 {
        param([string]$File, [double]$X0, [double]$Y0, [double]$X1, [double]$Y1)
        $bmp = New-Object System.Drawing.Bitmap($File)
        $x0 = [int]($bmp.Width * $X0); $y0 = [int]($bmp.Height * $Y0)
        $x1 = [int]($bmp.Width * $X1); $y1 = [int]($bmp.Height * $Y1)
        $sum = 0.0; $n = 0
        for ($y = $y0; $y -lt $y1; $y += 12) {
            for ($x = $x0; $x -lt $x1; $x += 12) {
                $p = $bmp.GetPixel($x, $y)
                $sum += ($p.R + $p.G + $p.B) / 3.0; $n++
            }
        }
        $bmp.Dispose()
        return [Math]::Round($sum / [Math]::Max(1, $n), 1)
    }
    foreach ($f in @("e14-0-after-storm.png","e14-1-after-storm-2s.png")) {
        $lum = Test-RegionBlackE14 -File (Join-Path $FramesDir $f) -X0 0.10 -Y0 0.10 -X1 0.50 -Y1 0.45
        Write-Host "[E14] $f video-region mean luminance = $lum" -ForegroundColor $(if ($lum -lt 12) { "Red" } else { "Green" })
        if ($lum -lt 12) { $global:E14Fail = $true }
    }

    $proc.Refresh()
    if ($proc.HasExited) { throw "E14: 进程提前退出 ($($proc.ExitCode))" }
} finally {
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    try {
        if ($sink) { $sink.Close() }
        if ($task.Wait(3000)) {
            $text = $task.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E14] stderr → $stderrLog" -ForegroundColor DarkCyan
            $storms = [regex]::Matches($text, "RESIZE-STORM n=(\d+) span_ms=(\d+) max_gap_ms=(\d+)")
            if ($storms.Count -eq 0) {
                Write-Host "[E14] FAIL: 未见 RESIZE-STORM 计数锚点(计数器未生效或风暴未触发)" -ForegroundColor Red
                $global:E14Fail = $true
            } else {
                $produced = 30 + 24
                $consumed = 0
                foreach ($m in $storms) { Write-Host "[E14] storm: n=$($m.Groups[1].Value) span_ms=$($m.Groups[2].Value) max_gap_ms=$($m.Groups[3].Value)" -ForegroundColor Cyan; $consumed += [int]$m.Groups[1].Value }
                Write-Host "[E14] consumed=$consumed / produced≈$produced (F1 合并生效度)" -ForegroundColor Cyan
            }
        }
    } catch {}
}
if ($global:E14Fail) { throw "E14 FAIL: 拖框风暴量化未达判据(详见 $stderrLog)" }
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
    $condId = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty,
        "player-enter-fullscreen"
    )
    $byId = $Root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condId)
    if ($byId) { return $byId }
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

function Find-DesktopPipButton {
    param([System.Windows.Automation.AutomationElement]$Root)
    $condBtn = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button
    )
    $btns = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condBtn)
    for ($i = 0; $i -lt $btns.Count; $i++) {
        $n = $btns[$i].Current.Name
        if ($n -eq "右下角小窗" -or $n -eq "小窗" -or $n -eq "还原主窗" -or $n -eq "主窗") {
            return $btns[$i]
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

# ---------- 场景 E10：全屏播放（native + mpv 双引擎：按钮进入 / 像素验收 / 退出恢复 / HUD 自隐 / 快捷键与双击） ----------
if (Should-RunScenario "E10") {
Write-Host "[E10] 全屏播放与退出走查（native + mpv 双引擎 + PrintWindow 像素验收）" -ForegroundColor Cyan
foreach ($mode in @(
    @{ Tag = "native"; EnvVal = "1"; WarmupMs = 5500; EnterWaitMs = 1900; RestoreWaitMs = 2200 },
    @{ Tag = "mpv";    EnvVal = "0"; WarmupMs = 4200; EnterWaitMs = 700;  RestoreWaitMs = 1000 }
)) {
    $tag = $mode.Tag
    Write-Host "[E10-$tag] 启动 QAQ_BX_NATIVE=$($mode.EnvVal)..." -ForegroundColor Cyan
    $env:QAQ_BX_VIDEO = "BV1GJ411x7h7"
    $env:QAQ_BX_NATIVE = $mode.EnvVal
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
            Write-Host "[E10-$tag] 注意：桌面不可抢焦点（可能已锁屏），将使用 UIAutomation InvokePattern 完成全屏点击校验" -ForegroundColor Yellow
        }
        Start-Sleep -Milliseconds $mode.WarmupMs # 等待媒体解析与首帧呈现

        $orig = Get-WindowGeometry -Hwnd $hwnd
        Capture-WindowFrame -Hwnd $hwnd -FileName "e17-before-fullscreen-$tag.png"
        $beforeSt = Measure-ImageRegionStats -File (Join-Path $FramesDir "e17-before-fullscreen-$tag.png") -X0 0.10 -Y0 0.12 -X1 0.50 -Y1 0.46
        Write-Host "[E10-$tag] before-fullscreen video mean=$($beforeSt.Mean) stddev=$($beforeSt.StdDev)" -ForegroundColor DarkCyan
        if ($beforeSt.Mean -le 15 -or $beforeSt.StdDev -lt 6) {
            throw "E10-$tag FAIL: 进入全屏前视频区域无有效画面 (mean=$($beforeSt.Mean) stddev=$($beforeSt.StdDev))"
        }

        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $fsBtn = Find-PlayerFullscreenButton -Root $root
        if (-not $fsBtn) { throw "E10-$tag FAIL: 播放页未定位到全屏按钮" }

        # 1. 点击播放页桌面播控条全屏按钮进入无边框全屏；先高速 Blit 到内存 Bitmap 并立刻点退出（仅 ~50ms，避开 3s HUD 自动隐藏），之后再 PNG 落盘并测亮
        $fsBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds $mode.EnterWaitMs
        Assert-FullscreenBounds -Hwnd $hwnd -StepName "$tag 进入全屏"
        $exitBtn = Find-HudExitButton -Root $root
        if (-not $exitBtn) { throw "E10-$tag FAIL: 进入全屏后顶部 HUD 未展示 [退出全屏] 按钮" }
        $fsBmp = Grab-WindowBitmap -Hwnd $hwnd

        # 2. 退出全屏（紧跟内存抓帧触发，远离 3s HUD 隐藏与 PNG 编码延迟）
        if ($interactive) {
            [UiEdgeNative]::keybd_event(0x1B, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 40
            [UiEdgeNative]::keybd_event(0x1B, 0, 2, [UIntPtr]::Zero)
        } else {
            $exited = $false
            try {
                $exitBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                $exited = $true
            } catch {}
            for ($attempt = 0; $attempt -lt 3 -and -not $exited; $attempt++) {
                Start-Sleep -Milliseconds 60
                $exitBtnNow = Find-HudExitButton -Root $root
                if ($exitBtnNow) {
                    try {
                        $exitBtnNow.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                        $exited = $true
                        break
                    } catch {}
                }
            }
            if (-not $exited) {
                $exitFallback = Find-HudExitButton -Root $root
                if (-not $exitFallback -or -not (Invoke-UiaElement -Element $exitFallback -What "退出全屏")) {
                    $fsBmp.Dispose()
                    throw "E10-$tag FAIL: 无法触发 [退出全屏] 按钮"
                }
            }
        }
        $fsHudPath = Join-Path $FramesDir "e18-fullscreen-hud-$tag.png"
        try {
            $fsBmp.Save($fsHudPath, [System.Drawing.Imaging.ImageFormat]::Png)
        } finally {
            $fsBmp.Dispose()
        }
        Write-Host "[shot] $fsHudPath" -ForegroundColor DarkCyan
        $fsHudSt = Measure-ImageRegionStats -File $fsHudPath -X0 0.20 -Y0 0.20 -X1 0.80 -Y1 0.80
        Write-Host "[E10-$tag] fullscreen HUD mean=$($fsHudSt.Mean) stddev=$($fsHudSt.StdDev)" -ForegroundColor $(if ($fsHudSt.Mean -le 15 -or $fsHudSt.StdDev -lt 6) { "Red" } else { "Green" })
        if ($fsHudSt.Mean -le 15 -or $fsHudSt.StdDev -lt 6) {
            throw "E10-$tag FAIL: 全屏状态中心区域无有效画面 (mean=$($fsHudSt.Mean) stddev=$($fsHudSt.StdDev))"
        }
        Start-Sleep -Milliseconds $mode.RestoreWaitMs
        $rest = Get-WindowGeometry -Hwnd $hwnd
        if ([Math]::Abs($rest.W - $orig.W) -gt 24 -or [Math]::Abs($rest.H - $orig.H) -gt 24 -or [Math]::Abs($rest.CW - $orig.CW) -gt 24) {
            throw "E10-$tag FAIL: 退出全屏后窗口尺寸未恢复 (期望 outer=$($orig.W)x$($orig.H) client=$($orig.CW)x$($orig.CH), 实际 outer=$($rest.W)x$($rest.H) client=$($rest.CW)x$($rest.CH))"
        }
        Capture-WindowFrame -Hwnd $hwnd -FileName "e20-after-fullscreen-restore-$tag.png"
        $restSt = Measure-ImageRegionStats -File (Join-Path $FramesDir "e20-after-fullscreen-restore-$tag.png") -X0 0.10 -Y0 0.12 -X1 0.50 -Y1 0.46
        Write-Host "[E10-$tag] restored video mean=$($restSt.Mean) stddev=$($restSt.StdDev)" -ForegroundColor $(if ($restSt.Mean -le 15 -or $restSt.StdDev -lt 6) { "Red" } else { "Green" })
        if ($restSt.Mean -le 15 -or $restSt.StdDev -lt 6) {
            throw "E10-$tag FAIL: 退出全屏后视频画面未恢复/无有效画面 (mean=$($restSt.Mean) stddev=$($restSt.StdDev))"
        }

        # 3. 再次进入全屏，静置 >3.6s 验证 HUD 自动隐藏 + 全屏画面持续有像素
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
            if (-not $fsBtn2) { throw "E10-$tag FAIL: 退出全屏恢复后播放页全屏按钮缺失" }
            $fsBtn2.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        }
        Start-Sleep -Milliseconds 700
        Assert-FullscreenBounds -Hwnd $hwnd -StepName "$tag 二次进入全屏"

        Start-Sleep -Milliseconds 3600
        $condBtn = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button
        )
        # 全屏现在保留底层播放页组件(避免 MPE 销毁换源),播放页播控条仍在 UIA 树中
        # 但已属性收起;自动隐藏只针对 HUD 覆盖层本身判定。
        $hudRoot = Find-UiaByAutomationId -Root $root -Id "fs-hud"
        if ($hudRoot) {
            $btnsHidden = $hudRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condBtn)
        } else {
            $btnsHidden = @()
        }
        $visBtns = 0
        foreach ($b in $btnsHidden) {
            $r = $b.Current.BoundingRectangle
            if (-not ([double]::IsInfinity($r.X) -or [double]::IsNaN($r.X))) { $visBtns++ }
        }
        if ($visBtns -ne 0) {
            throw "E10-$tag FAIL: 全屏静置后 HUD 未自动隐藏 (可见按钮数=$visBtns)"
        }
        Capture-WindowFrame -Hwnd $hwnd -FileName "e19-fullscreen-autohide-$tag.png"
        $fsHideSt = Measure-ImageRegionStats -File (Join-Path $FramesDir "e19-fullscreen-autohide-$tag.png") -X0 0.20 -Y0 0.20 -X1 0.80 -Y1 0.80
        Write-Host "[E10-$tag] fullscreen autohide mean=$($fsHideSt.Mean) stddev=$($fsHideSt.StdDev)" -ForegroundColor $(if ($fsHideSt.Mean -le 15 -or $fsHideSt.StdDev -lt 6) { "Red" } else { "Green" })
        if ($fsHideSt.Mean -le 15 -or $fsHideSt.StdDev -lt 6) {
            throw "E10-$tag FAIL: 全屏 HUD 隐藏后中心区域无有效画面 (mean=$($fsHideSt.Mean) stddev=$($fsHideSt.StdDev))"
        }

        # 4. 交互桌面下追加验证 F 键退出全屏恢复
        if ($interactive) {
            [UiEdgeNative]::keybd_event(0x46, 0, 0, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 40
            [UiEdgeNative]::keybd_event(0x46, 0, 2, [UIntPtr]::Zero)
            Start-Sleep -Milliseconds 800
            $rest2 = Get-WindowGeometry -Hwnd $hwnd
            if ([Math]::Abs($rest2.W - $orig.W) -gt 24 -or [Math]::Abs($rest2.H - $orig.H) -gt 24) {
                throw "E10-$tag FAIL: F 键退出全屏后窗口尺寸未恢复 ($($rest2.W)x$($rest2.H) vs $($orig.W)x$($orig.H))"
            }
        }

        $proc.Refresh()
        if ($proc.HasExited) { throw "E10-$tag FAIL: 全屏走查后进程意外退出 ($($proc.ExitCode))" }
        Write-Host "[E10-$tag] PASS (before=$($beforeSt.Mean)/sd=$($beforeSt.StdDev) fs_hud=$($fsHudSt.Mean)/sd=$($fsHudSt.StdDev) rest=$($restSt.Mean)/sd=$($restSt.StdDev) fs_hide=$($fsHideSt.Mean)/sd=$($fsHideSt.StdDev))" -ForegroundColor Green
    } finally {
        Remove-Item Env:QAQ_BX_VIDEO, Env:QAQ_BX_NATIVE -ErrorAction SilentlyContinue
        if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    }
}
}

# ---------- 场景 E15：设置页改「播放引擎」→ 下一次打开视频按新引擎装载（M7） ----------
# 口径（与 docs/Update-0.3.0.md M7 行一致）：本应用「离开播放页即关播」，改引擎时当前
# 流早已停止，所以切换只记账、绝不偷偷重开或打断在播通路；新引擎在**下一次打开视频**
# 生效。判据全走 stderr 锚点（截图像素在 TOPMOST/前台竞争下不可靠，见 dev-notes）：
# native 侧装载锚点必须整段落在「切换锚点之后」，且 native 生效段内不得再出现 mpv loadfile。
if (Should-RunScenario "E15") {
Write-Host "[E15] 播放引擎设置项（UIA 改引擎 -> 下一次打开按新引擎装载 -> 切回 mpv）" -ForegroundColor Cyan
$global:E15Fail = $false
$stderrLog = Join-Path $OutRoot "e15-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e15-bak"
$script:settingsSeeded = $false
$proc = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    # 无 BOM 写入：Set-Content -Encoding UTF8（PowerShell 5.1）带 BOM，serde 解析失败会
    # 整份退默认，场景起点就不确定；另外显式钉成 mpv，避免上一次走查留下的 native。
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":60,"webp_hint_shown":true,"player_engine":"Mpv"}'
    )
    $script:settingsSeeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_ROUTE"] = "popular"
    # 故意不设 QAQ_BX_NATIVE：引擎必须由设置项决定，这才是产品路径。
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    Start-Sleep -Seconds 6   # 热门页真实数据 + 首行卡片就位

    # 1) mpv 打开首卡（设置项默认引擎）
    Click-Fast -X ($winX + 330) -Y ($winY + 360)
    Start-Sleep -Seconds 16
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e15-0-mpv-playing.png"

    # 2) 设置页改引擎 -> 原生 Windows Media（只记账）
    if (-not (Select-UiaNav -Root $root -Name "设置")) { throw "E15 FAIL: 导航栏未找到「设置」项" }
    Start-Sleep -Seconds 3
    if (-not (Select-UiaEngineItem -Root $root -ItemPattern "原生")) { throw "E15 FAIL: 无法在设置页选中原生引擎" }
    Start-Sleep -Seconds 4
    Capture-WindowFrame -Hwnd $hwnd -FileName "e15-1-settings-native.png"

    # 3) 回热门页再开一张卡：这一次装载必须走系统媒体引擎
    if (-not (Select-UiaNav -Root $root -Name "热门")) { throw "E15 FAIL: 导航栏未找到「热门」项" }
    Start-Sleep -Seconds 4
    Click-Fast -X ($winX + 620) -Y ($winY + 360)
    Start-Sleep -Seconds 26   # 解析 + 装载线程 + MPE 注入开画 + 弹幕分段到货
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e15-2-native-playing.png"

    # 4) 切回 mpv：native 附属资源下线，下一次打开回到 mpv
    if (-not (Select-UiaNav -Root $root -Name "设置")) { throw "E15 FAIL: 导航栏未找到「设置」项(二次)" }
    Start-Sleep -Seconds 3
    if (-not (Select-UiaEngineItem -Root $root -ItemPattern "mpv")) { throw "E15 FAIL: 无法在设置页选回 mpv" }
    Start-Sleep -Seconds 4
    if (-not (Select-UiaNav -Root $root -Name "热门")) { throw "E15 FAIL: 导航栏未找到「热门」项(二次)" }
    Start-Sleep -Seconds 4
    Click-Fast -X ($winX + 900) -Y ($winY + 360)
    Start-Sleep -Seconds 18
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e15-3-back-to-mpv.png"

    $proc.Refresh()
    if ($proc.HasExited) { throw "E15: 进程提前退出 ($($proc.ExitCode))" }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask.Wait(5000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E15] stderr -> $stderrLog" -ForegroundColor DarkCyan
            # 按两个切换锚点把 stderr 切成三段：起点(mpv) / native 生效期 / 切回 mpv 之后。
            $partsN = @($text -split "player engine -> native")
            $fail = {
                param($msg)
                Write-Host "[E15] FAIL: $msg" -ForegroundColor Red
                $global:E15Fail = $true
            }
            if ($partsN.Count -lt 2) {
                & $fail "设置页没记账出 native 引擎锚点"
            } elseif ($text -notmatch "loadfile start=") {
                & $fail "起点没走 mpv loadfile（默认引擎不是 mpv）"
            } elseif ($text -match "native engine (init|load) failed|native build_source failed|native media failed|native engine attach failed|danmaku stage: (device|swapchain|begin_draw|present|ResizeBuffers)") {
                & $fail "native 通路装载/接管报错（见 stderr）"
            } else {
                $afterNative = $partsN[1]
                $partsM = @($afterNative -split "player engine -> mpv")
                $nativeWindow = $partsM[0]
                $afterMpv = if ($partsM.Count -gt 1) { $partsM[1] } else { $null }
                if ($nativeWindow -notmatch "native load qn=") {
                    & $fail "切换后的下一次打开没走系统媒体引擎装载"
                } elseif ($nativeWindow -match "loadfile start=") {
                    & $fail "native 生效期里又出现 mpv loadfile（引擎分流没翻转）"
                } elseif ($nativeWindow -notmatch "native build_source ok") {
                    & $fail "装载线程没产出 MediaSource"
                } elseif ($nativeWindow -notmatch "native attach") {
                    & $fail "MPE 未接管 player（视图分支未翻转）"
                } elseif ($nativeWindow -notmatch "native media opened") {
                    & $fail "换引擎后系统媒体管线没开画"
                } elseif ($nativeWindow -notmatch "native danmaku stage: 透明层已创建") {
                    & $fail "弹幕没随 native 通路建立透明层"
                } elseif ($nativeWindow -notmatch "STAGE-A: danmaku surface ok") {
                    & $fail "透明层 swapchain 未产出"
                } elseif ($nativeWindow -notmatch "smtc native: 控件已绑定") {
                    & $fail "SMTC 写权没交接给 player 自持控件"
                } elseif (-not $afterMpv) {
                    & $fail "没看到切回 mpv 的记账锚点"
                } elseif (-not ($text -match "native danmaku stage: 透明层已创建") -or $text.IndexOf("native danmaku stage: 已释放") -lt $text.IndexOf("native danmaku stage: 透明层已创建")) {
                    # 释放时机是「关播/切引擎」二选一(离开播放页即关播),所以只断言
                    # 「建过的透明层都被释放」,不锁死它在切回 mpv 锚点之后。
                    & $fail "弹幕透明层没有随 native 通路下线(层线程/D3D device 白占)"
                } elseif ($afterMpv -notmatch "loadfile start=") {
                    & $fail "切回 mpv 后下一次打开没走 mpv loadfile"
                } else {
                    Write-Host "[E15] PASS: 引擎设置只记账（不打断在播通路），下一次打开按新引擎装载，切回 mpv 后 native 附属资源全部下线" -ForegroundColor Green
                }
            }
        } else {
            Write-Host "[E15] FAIL: 没拿到 stderr" -ForegroundColor Red
            $global:E15Fail = $true
        }
    } catch {
        Write-Host "[E15] FAIL: stderr 判定异常 $($_.Exception.Message)" -ForegroundColor Red
        $global:E15Fail = $true
    }
}
if ($global:E15Fail) { throw "E15 FAIL: 播放引擎设置项验证失败（详见 $stderrLog）" }
}

# ---------- 场景 E17：tier1 —— 默认原生 + 后台播放(park) + 当次无缝切换引擎 ----------
# 不写 player_engine(老配置) ⇒ 默认必须走系统媒体引擎;进设置页(park 垫底)时
# heartbeat 继续推进 = 后台播放;切回 mpv 应同进度接管(loadfile start= 播放头)。
if (Should-RunScenario "E17") {
Write-Host "[E17] tier1：默认原生 + 后台播放 + 当次无缝切换" -ForegroundColor Cyan
$global:E17Fail = $false
$stderrLog = Join-Path $OutRoot "e17-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e16-bak"
$script:settingsSeeded = $false
$proc = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    # 老配置(不含 player_engine):默认引擎必须是原生。
    # window 字段必须显式给:无 window 字段时窗口会按上次残留尺寸恢复成 490x195
    # 之类的小窗,被 Wait-MainWindow 的 >=350x350 过滤拒掉(超时假象)。
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":60,"webp_hint_shown":true,"window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:settingsSeeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_ROUTE"] = "popular"
    # E17 验的是常驻层能力:park 产品默认关,场景内显式开启。
    $psi.EnvironmentVariables["QAQ_BX_PARK"] = "1"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    Start-Sleep -Seconds 6

    # 1) 默认引擎 = 原生:首卡直接走系统媒体引擎
    Click-Fast -X ($winX + 330) -Y ($winY + 360)
    Start-Sleep -Seconds 22
    Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e17-0-native-default.png"

    # 2) 进设置页(park 垫底):后台播放继续
    if (-not (Select-UiaNav -Root $root -Name "设置")) { throw "E17 FAIL: 未找到「设置」导航项" }
    Start-Sleep -Seconds 20
    Capture-WindowFrame -Hwnd $hwnd -FileName "e17-1-parked-settings.png"

    # 3) 当次无缝切换:native -> mpv(同进度接管)
    if (-not (Select-UiaEngineItem -Root $root -ItemPattern "mpv")) { throw "E17 FAIL: 无法切换到 mpv" }
    Start-Sleep -Seconds 12
    Capture-WindowFrame -Hwnd $hwnd -FileName "e17-2-switched-to-mpv.png"

    # 4) 再切回原生(注入在后台完成)
    if (-not (Select-UiaEngineItem -Root $root -ItemPattern "原生")) { throw "E17 FAIL: 无法切换回原生" }
    Start-Sleep -Seconds 12

    $proc.Refresh()
    if ($proc.HasExited) { throw "E17: 进程提前退出 ($($proc.ExitCode))" }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask.Wait(5000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E17] stderr -> $stderrLog" -ForegroundColor DarkCyan
            # 心跳推进断言:取全部 heartbeat pos,要求最后一个 > 第一个 + 20s(跨越 park 期)。
            $positions = [regex]::Matches($text, "native heartbeat pos=([0-9.]+)") | ForEach-Object { [double]$_.Groups[1].Value }
            $iSwitchToMpv = $text.IndexOf("player engine switch: native -> mpv")
            $iSwitchToNative = $text.IndexOf("player engine switch: mpv -> native")
            $afterMpv = if ($iSwitchToMpv -ge 0) { $text.Substring($iSwitchToMpv) } else { $null }
            $afterNative = if ($iSwitchToNative -ge 0) { $text.Substring($iSwitchToNative) } else { $null }
            $fail = {
                param($msg)
                Write-Host "[E17] FAIL: $msg" -ForegroundColor Red
                $global:E17Fail = $true
            }
            # 切 mpv 之前的段 = 起点:native 默认引擎,不得出现 mpv loadfile。
            $beforeSwitch = if ($iSwitchToMpv -ge 0) { $text.Substring(0, $iSwitchToMpv) } else { $text }
            # 切回 native 的锚点把 $afterMpv 再切一段:native 重新装载应落在其中。
            $partsBack = @($afterMpv -split "player engine switch: mpv -> native")
            $mpvWindow = $partsBack[0]
            $afterBack = if ($partsBack.Count -gt 1) { $partsBack[1] } else { $null }
            if ($beforeSwitch -match "loadfile start=") {
                & $fail "默认引擎不是原生(切 mpv 之前出现 mpv loadfile)"
            } elseif ($beforeSwitch -notmatch "native load qn=") {
                & $fail "默认引擎未走系统媒体引擎装载"
            } elseif ($positions.Count -lt 2 -or ($positions[-1] - $positions[0]) -lt 20) {
                & $fail "后台播放心跳未推进(park 期间播放停了)"
            } elseif ($iSwitchToMpv -lt 0) {
                & $fail "没看到 native -> mpv 的当次切换锚点"
            } elseif ($afterMpv -notmatch "loadfile start=([1-9][0-9]*\.[0-9])") {
                & $fail "切回 mpv 没有同进度接管(loadfile start 应带播放头小数)"
            } elseif ($afterMpv -notmatch "native danmaku stage: 已释放") {
                & $fail "切回 mpv 后透明层没下线"
            } elseif ($partsBack.Count -lt 2) {
                & $fail "没看到 mpv -> native 的当次切换锚点"
            } elseif ($afterBack -notmatch "native load qn=") {
                & $fail "切回原生没有重新装载"
            } elseif ($text -match "native media failed|native engine (init|load) failed|native build_source failed|native engine attach failed") {
                & $fail "native 通路报错(见 stderr)"
            } else {
                Write-Host "[E17] PASS: 默认原生 + 后台播放(park 心跳推进) + 双向当次无缝切换(同进度接管)" -ForegroundColor Green
            }
        } else {
            Write-Host "[E17] FAIL: 没拿到 stderr" -ForegroundColor Red
            $global:E17Fail = $true
        }
    } catch {
        Write-Host "[E17] FAIL: stderr 判定异常 $($_.Exception.Message)" -ForegroundColor Red
        $global:E17Fail = $true
    }
}
if ($global:E17Fail) { throw "E17 FAIL: tier1 验证失败(详见 $stderrLog)" }
}

Write-Host "[done] 极端场景走查完成，截图位于 $FramesDir" -ForegroundColor Green

# ---------- 场景 E18:音量条悬停滚轮调节(每格 ±5) ----------
# 判据(UIA,不依赖像素):音量条 = RangeValue.Max==100 的 Slider(进度条 max=视频时长秒);
# 滚轮上滚一格 → 60→65(% 文本同步),下滚一格 → 65→60。种子三件套照抄 E15/E17。
function Find-PlayerVolumeSlider {
    param([System.Windows.Automation.AutomationElement]$Root)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Slider
    )
    $sliders = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
    for ($i = 0; $i -lt $sliders.Count; $i++) {
        try {
            $range = $sliders[$i].GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern)
            if ([Math]::Abs($range.Current.Maximum - 100.0) -lt 0.01 -and [Math]::Abs($range.Current.Minimum) -lt 0.01) {
                return $sliders[$i]
            }
        } catch {}
    }
    return $null
}

function Find-VolumePercentText {
    # 音量条旁的 "N%" 文本:限定在音量条右侧 90px 内,避免误吃右栏其它数字。
    param([System.Windows.Automation.AutomationElement]$Root, [System.Windows.Automation.AutomationElement]$Slider)
    $condText = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text
    )
    $texts = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condText)
    $srect = $Slider.Current.BoundingRectangle
    for ($i = 0; $i -lt $texts.Count; $i++) {
        $name = $texts[$i].Current.Name
        if ($name -match '^\d{1,3}%$') {
            $trect = $texts[$i].Current.BoundingRectangle
            if ($trect.Left -ge $srect.Left - 8 -and $trect.Left -le $srect.Right + 90 -and
                [Math]::Abs(($trect.Top + $trect.Bottom) / 2 - ($srect.Top + $srect.Bottom) / 2) -lt 40) {
                return $texts[$i]
            }
        }
    }
    return $null
}

function Wait-VolumeValue {
    param([System.Windows.Automation.RangeValuePattern]$Range, [double]$Expect, [int]$TimeoutMs)
    for ($i = 0; $i -lt ($TimeoutMs / 200); $i++) {
        Start-Sleep -Milliseconds 200
        try {
            if ([Math]::Abs([double]$Range.Current.Value - $Expect) -lt 1.5) { return $true }
        } catch {}
    }
    return $false
}

if (Should-RunScenario "E18") {
Write-Host "[E18] 音量条悬停滚轮调节(60 → 65 → 60)" -ForegroundColor Cyan
$global:E18Fail = $false
$stderrLog = Join-Path $OutRoot "e18-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e18-bak"
$script:settingsSeeded = $false
$proc = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    # 种子三件套(无 BOM / webp_hint_shown / 显式 window) + 音量 60 起点 + mpv 引擎。
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":60,"webp_hint_shown":true,"player_engine":"Mpv","window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:settingsSeeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_VIDEO"] = "BV1GJ411x7h7"
    $psi.EnvironmentVariables["QAQ_BX_NATIVE"] = "0"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Milliseconds 4500 # 起播出画,播控条就位
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

    $vol = Find-PlayerVolumeSlider -Root $root
    if (-not $vol) { throw "E18 FAIL: 播放页未定位到音量条(Max==100 的 Slider)" }
    $range = $vol.GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern)
    $initial = [double]$range.Current.Value
    if ([Math]::Abs($initial - 60) -gt 2) { throw "E18 FAIL: 种子音量未生效(期望≈60,实际 $initial)" }
    $volText = Find-VolumePercentText -Root $root -Slider $vol
    if (-not $volText) { throw "E18 FAIL: 音量条旁未定位到 N% 文本" }
    Write-Host "[E18] 起点 volume=$($range.Current.Value) text='$($volText.Current.Name)'" -ForegroundColor DarkCyan

    # 1) 悬停滚轮上滚一格:60 → 65(% 文本必须同步)
    $r = $vol.Current.BoundingRectangle
    [void][UiEdgeNative]::SetForegroundWindow($hwnd)
    [void][UiEdgeNative]::SetCursorPos([int](($r.Left + $r.Right) / 2), [int](($r.Top + $r.Bottom) / 2))
    Start-Sleep -Milliseconds 260
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_WHEEL, 0, 0, 120, [UIntPtr]::Zero)
    if (-not (Wait-VolumeValue -Range $range -Expect 65 -TimeoutMs 5000)) {
        throw "E18 FAIL: 滚轮上滚后音量未变 65(实际 $($range.Current.Value))"
    }
    $upTextOk = $false
    for ($i = 0; $i -lt 10 -and -not $upTextOk; $i++) {
        Start-Sleep -Milliseconds 200
        $volText = Find-VolumePercentText -Root $root -Slider $vol
        if ($volText -and $volText.Current.Name -eq "65%") { $upTextOk = $true }
    }
    if (-not $upTextOk) { throw "E18 FAIL: 上滚后 % 文本未同步为 65%" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e18-1-wheel-up-65.png"

    # 2) 下滚一格:65 → 60
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_WHEEL, 0, 0, -120, [UIntPtr]::Zero)
    if (-not (Wait-VolumeValue -Range $range -Expect 60 -TimeoutMs 5000)) {
        throw "E18 FAIL: 滚轮下滚后音量未回 60(实际 $($range.Current.Value))"
    }
    $downTextOk = $false
    for ($i = 0; $i -lt 10 -and -not $downTextOk; $i++) {
        Start-Sleep -Milliseconds 200
        $volText = Find-VolumePercentText -Root $root -Slider $vol
        if ($volText -and $volText.Current.Name -eq "60%") { $downTextOk = $true }
    }
    if (-not $downTextOk) { throw "E18 FAIL: 下滚后 % 文本未同步回 60%" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e18-2-wheel-down-60.png"

    $proc.Refresh()
    if ($proc.HasExited) { throw "E18: 进程提前退出 ($($proc.ExitCode))" }
    Write-Host "[E18] PASS: 音量条悬停滚轮 60→65→60,% 文本同步" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask.Wait(3000)) { $stderrTask.Result | Set-Content -Path $stderrLog -Encoding UTF8; Write-Host "[E18] stderr -> $stderrLog" -ForegroundColor DarkCyan }
    } catch {}
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
}
if ($global:E18Fail) { throw "E18 FAIL: 音量条滚轮验证失败(详见 $stderrLog)" }
}

# ---------- 场景 E19:进度条拖拽提示(label)显示真实播放时间 ----------
# 判据(UIA):按住进度条拖动期间,滑块上方出现形如 mm:ss(或 h:mm:ss)的独立时间文本,
# 且其数值与拖动位置的 RangeValue 对应 —— 替换掉原生 1..N 裸数值 tooltip。
function Find-SeekSlider {
    param([System.Windows.Automation.AutomationElement]$Root)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Slider
    )
    $sliders = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
    for ($i = 0; $i -lt $sliders.Count; $i++) {
        try {
            $range = $sliders[$i].GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern)
            # 进度条 max=视频时长秒(BV1GJ411x7h7 ≈ 213s),音量条 max=100。
            if ($range.Current.Maximum -gt 105.0) { return $sliders[$i] }
        } catch {}
    }
    return $null
}

function Find-DragTimeTooltip {
    # 拖动中滑块上方的时间文本:限定在进度条横向范围内、其正上方 90px 内。
    param([System.Windows.Automation.AutomationElement]$Root, [System.Windows.Automation.AutomationElement]$Slider)
    $condText = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text
    )
    $texts = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condText)
    $srect = $Slider.Current.BoundingRectangle
    for ($i = 0; $i -lt $texts.Count; $i++) {
        $name = $texts[$i].Current.Name
        if ($name -match '^\d{1,2}:[0-5]\d$' -or $name -match '^\d+:[0-5]\d:[0-5]\d$') {
            $trect = $texts[$i].Current.BoundingRectangle
            $cx = ($trect.Left + $trect.Right) / 2
            if ($cx -ge $srect.Left - 20 -and $cx -le $srect.Right + 20 -and
                $trect.Bottom -le $srect.Top + 12 -and $trect.Bottom -ge $srect.Top - 90) {
                return $texts[$i]
            }
        }
    }
    return $null
}

function Convert-SeekTooltipExpect {
    # 与 fork converters.rs 的 Duration 格式一致:秒(round-half-up)→ mm:ss / h:mm:ss。
    param([double]$Seconds)
    $secs = [int][Math]::Floor([Math]::Max(0.0, $Seconds) + 0.5)
    $h = [int][Math]::Floor($secs / 3600)
    $m = [int][Math]::Floor(($secs % 3600) / 60)
    $s = $secs % 60
    if ($h -gt 0) { return "{0}:{1:d2}:{2:d2}" -f $h, $m, $s }
    return "{0:d2}:{1:d2}" -f $m, $s
}

if (Should-RunScenario "E19") {
Write-Host "[E19] 进度条拖拽提示显示真实播放时间(替换 1..N 数值)" -ForegroundColor Cyan
$global:E19Fail = $false
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e19-bak"
$script:settingsSeeded = $false
$proc = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":60,"webp_hint_shown":true,"player_engine":"Mpv","window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:settingsSeeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_VIDEO"] = "BV1GJ411x7h7"
    $psi.EnvironmentVariables["QAQ_BX_NATIVE"] = "0"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $null = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0010)
    Start-Sleep -Milliseconds 4500
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

    $seek = Find-SeekSlider -Root $root
    if (-not $seek) { throw "E19 FAIL: 未定位到进度条(max>105 的 Slider)" }
    $range = $seek.GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern)
    $duration = [double]$range.Current.Maximum
    $srect = $seek.Current.BoundingRectangle

    # 真实拖拽:按住轨道 25% 处 → 分步移到 45% → 持住期间轮询 tooltip。
    [void][UiEdgeNative]::SetForegroundWindow($hwnd)
    $startX = [int]($srect.Left + ($srect.Right - $srect.Left) * 0.25)
    $endX = [int]($srect.Left + ($srect.Right - $srect.Left) * 0.45)
    $dragY = [int](($srect.Top + $srect.Bottom) / 2)
    [void][UiEdgeNative]::SetCursorPos($startX, $dragY)
    Start-Sleep -Milliseconds 260
    [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_LEFTDOWN, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
    try {
        $tooltip = $null
        for ($i = 1; $i -le 14 -and -not $tooltip; $i++) {
            $x = [int]($startX + ($endX - $startX) * ($i / 14.0))
            [void][UiEdgeNative]::SetCursorPos($x, $dragY)
            Start-Sleep -Milliseconds 90
            $tooltip = Find-DragTimeTooltip -Root $root -Slider $seek
        }
        if (-not $tooltip) {
            throw "E19 FAIL: 拖动进度条期间未在滑块上方发现 mm:ss 形态的时间提示(仍是原生数值 tooltip 或未渲染)"
        }
        $tipName = $tooltip.Current.Name
        Start-Sleep -Milliseconds 250
        $tooltip2 = Find-DragTimeTooltip -Root $root -Slider $seek
        $tipName2 = if ($tooltip2) { $tooltip2.Current.Name } else { $tipName }
        $expect = Convert-SeekTooltipExpect -Seconds ([double]$range.Current.Value)
        Write-Host "[E19] tooltip='$tipName'/'$tipName2' range=$([Math]::Round([double]$range.Current.Value,1))s expect≈$expect" -ForegroundColor DarkCyan
        if ($tipName2 -ne $expect) {
            throw "E19 FAIL: 静止后 tooltip '$tipName2' 与拖动位置 $([Math]::Round([double]$range.Current.Value,1))s 的期望值 '$expect' 不一致"
        }
        Capture-WindowFrame -Hwnd $hwnd -Mode screen -FileName "e19-1-seek-tooltip-time.png"
    } finally {
        [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_LEFTUP, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 400
    }

    $proc.Refresh()
    if ($proc.HasExited) { throw "E19: 进程提前退出 ($($proc.ExitCode))" }
    Write-Host "[E19] PASS: 进度条拖拽提示为真实播放时间(mm:ss)" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
    } catch {}
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
}
if ($global:E19Fail) { throw "E19 FAIL: 进度条拖拽提示验证失败" }
}
# ---------- 场景 E16:退出全屏→拖拽标题栏风暴(0x359595 幽灵回调修复回归,§8.4) ----------
if (Should-RunScenario "E16") {
Write-Host "[E16] 退出全屏后立刻拖拽标题条 20 次(WindowRectChanged 幽灵路径防护)" -ForegroundColor Cyan
$env:QAQ_BX_VIDEO = "BV1GJ411x7h7"
$dumpDir = Join-Path (Split-Path (Split-Path $PSScriptRoot -Parent) -Parent) "data\crash-dumps"
$dumpBefore = @(Get-ChildItem $dumpDir -Filter "qaq-bx-ui.exe.*.dmp" -ErrorAction SilentlyContinue).Count
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 4200 # 等媒体解析与首帧(同 E10)

    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $fsBtn = Find-PlayerFullscreenButton -Root $root
    if (-not $fsBtn) { throw "E16 FAIL: 播放页未定位到全屏按钮" }
    $fsBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 600
    Assert-FullscreenBounds -Hwnd $hwnd -StepName "进入全屏"
    $exitBtn = Find-HudExitButton -Root $root
    if (-not $exitBtn) { throw "E16 FAIL: HUD 未展示退出全屏按钮" }
    $exitBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()

    # 退出全屏后立刻真实鼠标拖框(经 user32 modal 循环,复现死亡窗口事件风暴)。
    for ($i = 1; $i -le 20; $i++) {
        $g = Get-WindowGeometry -Hwnd $hwnd
        # 起点取右侧副标题空带(避开左侧返回/分栏按钮与系统按钮簇),y 在 Tall 标题条内
        $grabX = $g.X + $g.W - 320
        $grabY = $g.Y + 24
        $dx = if ($i % 2) { 420 } else { -420 }
        Drag-WindowEdge -StartX $grabX -StartY $grabY -EndX ($grabX + $dx) -EndY $grabY
        if ($proc.HasExited) { throw "E16 FAIL: 拖框 #$i 进程退出(exit=$($proc.ExitCode)),幽灵回调路径回归" }
    }
    Start-Sleep -Milliseconds 600
    if ($proc.HasExited) { throw "E16 FAIL: 进程迟滞退出(exit=$($proc.ExitCode))" }
    $g2 = Get-WindowGeometry -Hwnd $hwnd
    if ([Math]::Abs($g2.X - $g.X) -lt 40) {
        Write-Host "[E16] 注意:拖框后窗口位移不显著($($g.X)→$($g2.X)),拖拽可能未命中可拖区(桌面非前台?)" -ForegroundColor Yellow
    }
    $dumpAfter = @(Get-ChildItem $dumpDir -Filter "qaq-bx-ui.exe.*.dmp" -ErrorAction SilentlyContinue).Count
    if ($dumpAfter -ne $dumpBefore) { throw "E16 FAIL: 拖框过程新增崩溃 dump($dumpBefore→$dumpAfter)" }
    Write-Host "[E16] PASS: 退出全屏→20 次标题条拖框,进程存活、无新 dump" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
}
}

# ---------- 场景 E20：HalfPane 半窗预览 + Windows 原生视频引擎出画（Bug 2C） ----------
if (Should-RunScenario "E20") {
Write-Host "[E20] HalfPane 半窗预览下 Windows 原生视频引擎出画验证（QAQ_BX_NATIVE=1）" -ForegroundColor Cyan
$global:E20Fail = $false
$stderrLog = Join-Path $OutRoot "e20-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e20-bak"
$script:settingsSeeded = $false
$proc = $null
$stderrSink = $null
$stderrTask = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"HalfPane","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":60,"webp_hint_shown":true,"player_engine":"Native","window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:settingsSeeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables.Remove("QAQ_BX_VIDEO")
    $psi.EnvironmentVariables["QAQ_BX_NATIVE"] = "1"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    for ($i = 0; $i -lt 5; $i++) {
        if ([UiEdgeNative]::GetForegroundWindow() -eq $hwnd) { break }
        [UiEdgeNative]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        [UiEdgeNative]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        [void][UiEdgeNative]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep -Milliseconds 3500 # 等推荐网格首行渲染

    $rect = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$rect)
    $winX = $rect.Left; $winY = $rect.Top
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $condBtn = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button
    )
    $buttons = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condBtn)
    $cardBtn = $null
    for ($i = 0; $i -lt $buttons.Count; $i++) {
        $br = $buttons[$i].Current.BoundingRectangle
        if (-not ([double]::IsInfinity($br.X) -or [double]::IsNaN($br.X)) -and
            $br.Top -ge ($rect.Top + 180) -and $br.Width -ge 120 -and $br.Height -ge 100) {
            $cardBtn = $buttons[$i]
            break
        }
    }
    if ($cardBtn) {
        Write-Host "[E20] UIA 触发首页卡片按钮..." -ForegroundColor DarkCyan
        $cardBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    } else {
        Click-Fast -X ($winX + 260) -Y ($winY + 340)
    }
    Start-Sleep -Milliseconds 6500 # 半窗抽屉打开 + DASH 解析 + native 建源/attach/首帧

    Capture-WindowFrame -Hwnd $hwnd -FileName "e20-halfpane-native.png"
    $hpSt = Measure-ImageRegionStats -File (Join-Path $FramesDir "e20-halfpane-native.png") -X0 0.68 -Y0 0.14 -X1 0.96 -Y1 0.38
    Write-Host "[E20] half-pane native video mean=$($hpSt.Mean) stddev=$($hpSt.StdDev)" -ForegroundColor $(if ($hpSt.Mean -le 15 -or $hpSt.StdDev -lt 6) { "Red" } else { "Green" })
    if ($hpSt.Mean -le 15 -or $hpSt.StdDev -lt 6) {
        Write-Host "[E20] FAIL: HalfPane 原生视频区无有效画面 (mean=$($hpSt.Mean) stddev=$($hpSt.StdDev))" -ForegroundColor Red
        $global:E20Fail = $true
    }
    $proc.Refresh()
    if ($proc.HasExited) { throw "E20 FAIL: 进程提前退出 ($($proc.ExitCode))" }
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask -and $stderrTask.Wait(3000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E20] stderr -> $stderrLog" -ForegroundColor DarkCyan
            if ($text -match "native media failed|native build_source failed|native engine attach failed") {
                Write-Host "[E20] FAIL: native 通路出现错误锚点" -ForegroundColor Red
                $global:E20Fail = $true
            } elseif ($text -notmatch "native load qn=" -or $text -notmatch "native attach" -or $text -notmatch "native media opened") {
                Write-Host "[E20] FAIL: HalfPane 未完成 native 装载/attach/开画链路" -ForegroundColor Red
                $global:E20Fail = $true
            } elseif (-not $global:E20Fail) {
                Write-Host "[E20] PASS: HalfPane 原生视频装载 + attach + 开画 + 像素验收通过" -ForegroundColor Green
            }
        } else {
            Write-Host "[E20] FAIL: 未采到 stderr" -ForegroundColor Red
            $global:E20Fail = $true
        }
    } catch {
        Write-Host "[E20] FAIL: stderr 解析异常 $($_.Exception.Message)" -ForegroundColor Red
        $global:E20Fail = $true
    }
}
if ($global:E20Fail) { throw "E20 FAIL: HalfPane native 验证失败（详见 $stderrLog）" }
}

# ---------- 场景 E21：竖屏模式（实验）全屏进退彻底隔离与零闪回 Desktop 验证 ----------
if (Should-RunScenario "E21") {
Write-Host "[E21] 竖屏模式（实验）进出全屏严格隔离与零 Desktop 闪回验证" -ForegroundColor Cyan
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e21-bak"
$script:settingsSeeded = $false
$proc = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":50,"webp_hint_shown":true,"player_engine":"Native","portrait_mode":true}'
    )
    $script:settingsSeeded = $true
    $env:QAQ_BX_VIDEO = "BV1GJ411x7h7"

    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    for ($i = 0; $i -lt 5; $i++) {
        if ([UiEdgeNative]::GetForegroundWindow() -eq $hwnd) { break }
        [UiEdgeNative]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        [UiEdgeNative]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        [void][UiEdgeNative]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep -Milliseconds 5000

    $g0 = Get-WindowGeometry -Hwnd $hwnd
    $dpi = [Math]::Max(96, [int][UiEdgeNative]::GetDpiForWindow($hwnd))
    $scale = $dpi / 96.0
    $clientDips0 = [Math]::Round($g0.CW / $scale, 1)
    Write-Host "[E21] launch portrait geometry: outer=$($g0.W)x$($g0.H) client=$($g0.CW)x$($g0.CH) (${clientDips0} DIP)" -ForegroundColor DarkCyan
    if ($clientDips0 -ge 860.0) {
        throw "E21 FAIL: 竖屏启动客户区宽度越过 860 DIP 边界 ($clientDips0 DIP)"
    }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e21-0-portrait-before-fs.png"

    # 通过 UIAutomation InvokePattern 点击竖屏播放页全屏按钮进入全屏（不依赖前台焦点与屏幕坐标）
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $desktopPip0 = Find-DesktopPipButton -Root $root
    if ($desktopPip0) {
        throw "E21 FAIL: 竖屏模式播放页出现了桌面端专属小窗按钮"
    }
    $qualityHeaderLabel = Find-UiaByName -Root $root -Name "画质"
    if ($qualityHeaderLabel) {
        throw "E21 FAIL: 竖屏模式视频页面仍然存在解释性文本 [画质]"
    }
    $qualityCombo = Find-UiaByAutomationId -Root $root -Id "player-quality-selector"
    if (-not $qualityCombo) {
        throw "E21 FAIL: 竖屏模式播放页未找到 [player-quality-selector] 清晰度按钮"
    }
    if ($qualityCombo.Current.Name -match "画质") {
        throw "E21 FAIL: 竖屏模式清晰度 ComboBox Name 仍包含 [画质] ($($qualityCombo.Current.Name))"
    }
    # 自动化点击展开清晰度下拉并选第二项验证无 panic
    try {
        $expand = $qualityCombo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
        $expand.Expand()
        Start-Sleep -Milliseconds 450
        $condItem = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::ListItem
        )
        $qItems = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condItem)
        if ($qItems.Count -ge 2) {
            Write-Host "[E21] quality selector expanded ($($qItems.Count) items), selecting item[1]=$($qItems[1].Current.Name)" -ForegroundColor DarkCyan
            $qItems[1].GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
            Start-Sleep -Milliseconds 600
        } else {
            $expand.Collapse()
        }
    } catch {
        throw "E21 FAIL: 竖屏清晰度选择器展开/点击异常: $($_.Exception.Message)"
    }
    $fsBtn = Find-PlayerFullscreenButton -Root $root
    if (-not $fsBtn) { throw "E21 FAIL: 竖屏播放页未找到 [player-enter-fullscreen] 按钮" }
    $fsBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 1200

    $exitBtn = Find-HudExitButton -Root $root
    Assert-FullscreenBounds -Hwnd $hwnd -StepName "E21 竖屏进入全屏"
    if (-not $exitBtn) { throw "E21 FAIL: 进入全屏后未找到 HUD [退出全屏] 按钮" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e21-1-portrait-in-fs.png"

    # 点击退出全屏，立刻高频轮询 UIA 与窗口宽度：绝不允许出现桌面端 [右下角小窗] 按钮，且恢复后宽度必须 <= 859 DIP
    $exitBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    for ($tick = 0; $tick -lt 12; $tick++) {
        $desktopPipBtn = Find-DesktopPipButton -Root $root
        if ($desktopPipBtn) {
            throw "E21 FAIL: 退出全屏过渡期(tick=$tick)闪回了桌面端双栏 UI ([右下角小窗] 控件出现)"
        }
        Start-Sleep -Milliseconds 45
    }
    Start-Sleep -Milliseconds 600
    $g1 = Get-WindowGeometry -Hwnd $hwnd
    $clientDips1 = [Math]::Round($g1.CW / $scale, 1)
    Write-Host "[E21] after exit-fullscreen geometry: outer=$($g1.W)x$($g1.H) client=$($g1.CW)x$($g1.CH) (${clientDips1} DIP)" -ForegroundColor DarkCyan
    if ($clientDips1 -ge 860.0) {
        throw "E21 FAIL: 退出全屏后窗口宽度未保持在竖屏带 (< 860 DIP)，实际=${clientDips1} DIP"
    }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e21-2-portrait-after-fs.png"

    # 验证退出全屏后 presenter 宽度上限硬钳制仍在：尝试用 SetWindowPos 强行拉宽至 1400px，客户区仍须 < 860 DIP
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $g1.X, $g1.Y, 1400, $g1.H, [UiEdgeNative]::SWP_NOZORDER)
    Start-Sleep -Milliseconds 350
    $g2 = Get-WindowGeometry -Hwnd $hwnd
    $clientDips2 = [Math]::Round($g2.CW / $scale, 1)
    Write-Host "[E21] after forced 1400px resize attempt: outer=$($g2.W)x$($g2.H) client=$($g2.CW)x$($g2.CH) (${clientDips2} DIP)" -ForegroundColor DarkCyan
    if ($clientDips2 -ge 860.0) {
        throw "E21 FAIL: 退出全屏后 presenter 最大宽度硬钳制丢失，窗口被拉宽至 ${clientDips2} DIP"
    }

    $proc.Refresh()
    if ($proc.HasExited) { throw "E21 FAIL: 进程意外退出 ($($proc.ExitCode))" }
    Write-Host "[E21] PASS: 竖屏模式进退全屏零 Desktop 闪回 + 几何与 presenter 硬钳制保持完好" -ForegroundColor Green
} finally {
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
}
}

# ---------- 场景 E22：多 CDN 择优 + 本地并发预加载代理 + Native 加载指示自动化点击验收 ----------
if (Should-RunScenario "E22") {
Write-Host "[E22] 设置页 CDN/预加载开关 UIA 点击 + Native 走本地流代理实播/切清晰度验证" -ForegroundColor Cyan
$stderrLog = Join-Path $OutRoot "e22-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e22-bak"
$script:settingsSeeded = $false
$proc = $null
$stderrSink = $null
$stderrTask = $null
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":50,"webp_hint_shown":true,"player_engine":"Native","window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:settingsSeeded = $true

    # 阶段 A：设置页自动化点击开关（优先官方 CDN / 多线程并发预加载），确认无解释性文本、可连续拨动不崩溃
    $env:QAQ_BX_ROUTE = "settings"
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 1500
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

    $cdnToggle = Find-UiaByAutomationId -Root $root -Id "settings-prefer-official-cdn"
    $preloadToggle = Find-UiaByAutomationId -Root $root -Id "settings-multi-thread-preload"
    if (-not $cdnToggle) { throw "E22 FAIL: 设置页未找到 [settings-prefer-official-cdn] 开关" }
    if (-not $preloadToggle) { throw "E22 FAIL: 设置页未找到 [settings-multi-thread-preload] 开关" }

    foreach ($t in @($cdnToggle, $preloadToggle)) {
        $pat = $t.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
        if ($pat.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) {
            throw "E22 FAIL: 新设置开关默认值应为 On ($($t.Current.AutomationId))"
        }
        $pat.Toggle()
        Start-Sleep -Milliseconds 150
        if ($pat.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::Off) {
            throw "E22 FAIL: UIA 点击后开关未翻为 Off ($($t.Current.AutomationId))"
        }
        $pat.Toggle()
        Start-Sleep -Milliseconds 150
        if ($pat.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) {
            throw "E22 FAIL: UIA 再次点击后开关未翻回 On ($($t.Current.AutomationId))"
        }
    }
    # 等待 300ms SETTINGS_SAVE_DEBOUNCE 落盘完成后再退出，确保阶段 B 读到恢复后的 On 状态
    Start-Sleep -Milliseconds 650
    Capture-WindowFrame -Hwnd $hwnd -FileName "e22-0-settings-toggles.png"
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    $proc = $null

    # 阶段 B：默认 Native 引擎直开播放页，走本地 StreamProxy 代理实播 + UIA 切画质 + 画面像素检验
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_VIDEO"] = "BV1GJ411x7h7"
    $psi.EnvironmentVariables.Remove("QAQ_BX_ROUTE") | Out-Null
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Seconds 7
    Capture-WindowFrame -Hwnd $hwnd -FileName "e22-1-native-proxy-playing.png"
    $stats = Measure-ImageRegionStats -File (Join-Path $FramesDir "e22-1-native-proxy-playing.png") -X0 0.08 -Y0 0.14 -X1 0.62 -Y1 0.56
    Write-Host "[E22] video region mean=$($stats.Mean) stddev=$($stats.StdDev)" -ForegroundColor DarkCyan
    if ($stats.Mean -le 15 -or $stats.StdDev -lt 6) {
        throw "E22 FAIL: Native 走代理播放未出有效视频画面 (mean=$($stats.Mean) stddev=$($stats.StdDev))"
    }

    # UIA 点击画质选择器切换画质，验证换流链路及加载指示不崩溃
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $qualityCombo = Find-UiaByAutomationId -Root $root -Id "player-quality-selector"
    if ($qualityCombo) {
        $expand = $qualityCombo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
        $expand.Expand()
        Start-Sleep -Milliseconds 450
        $condItem = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::ListItem
        )
        $qItems = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condItem)
        if ($qItems.Count -ge 2) {
            Write-Host "[E22] switching quality via UIA -> $($qItems[1].Current.Name)" -ForegroundColor DarkCyan
            $qItems[1].GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
            Start-Sleep -Seconds 5
        } else {
            $expand.Collapse()
        }
    }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e22-2-after-quality-switch.png"
    $proc.Refresh()
    if ($proc.HasExited) { throw "E22 FAIL: 进程意外退出 ($($proc.ExitCode))" }
} finally {
    Remove-Item Env:QAQ_BX_ROUTE -ErrorAction SilentlyContinue
    Remove-Item Env:QAQ_BX_VIDEO -ErrorAction SilentlyContinue
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask -and $stderrTask.Wait(5000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E22] stderr -> $stderrLog" -ForegroundColor DarkCyan
            if ($text -notmatch "\[stream-proxy\] listening on http://127\.0\.0\.1:") {
                throw "E22 FAIL: 未看到 stream-proxy 监听日志"
            }
            if ($text -notmatch "stream-proxy route .*prefer_upos=true") {
                throw "E22 FAIL: 未看到 stream-proxy 走 prefer_upos=true 路由"
            }
            if ($text -notmatch "native media opened") {
                throw "E22 FAIL: Native 引擎经代理播放未触发 media opened"
            }
        }
    } catch {
        throw
    }
    Write-Host "[E22] PASS: 设置开关 UIA 点击 + 多 CDN/本地并发代理实播 + 切清晰度全链通过" -ForegroundColor Green
}
}

# ---------- 场景 E23：#7 卡片悬停 1s 弹出预览卡（真实鼠标 hover + UIA 点「播放」） ----------
if (Should-RunScenario "E23") {
Write-Host "[E23] 悬停预览卡：SetCursorPos hover -> peek-card 弹出 -> ✕ 单卡静默/换卡恢复 -> UIA Invoke 播放 -> 快速掠过 10 点不弹出" -ForegroundColor Cyan
$stderrLog = Join-Path $OutRoot "e23-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e23-bak"
$script:settingsSeeded = $false
$proc = $null
$stderrSink = $null
$stderrTask = $null
try {
    if (-not (Test-Path $dataDir)) { New-Item -ItemType Directory -Force -Path $dataDir | Out-Null }
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","autoplay":true,"hardware_decode":true,"volume":30,"player_engine":"Mpv","hover_preview":true,"window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:settingsSeeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables["QAQ_BX_TRACE_HOME"] = "1"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, 60, 60, 1296, 808, [UiEdgeNative]::SWP_NOZORDER)

    # 等首页流到货并经 UIA 找到真实卡片(宽>150 的 Button 即为视频卡命中盒)。
    $cards = @()
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 1000
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $cond = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button
        )
        $cards = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond) |
            Where-Object { $_.Current.BoundingRectangle.Width -gt 150 -and $_.Current.BoundingRectangle.Height -gt 120 })
        if ($cards.Count -ge 10) { break }
    }
    if ($cards.Count -lt 10) { throw "E23 FAIL: 首页可悬停视频卡不足 10 张($($cards.Count))" }
    $cards = $cards | Sort-Object { $_.Current.BoundingRectangle.Top }, { $_.Current.BoundingRectangle.Left }

    # ① 快速掠过 ≤10 张卡中心(每点 ~70ms):任何一张不满 1s,全程不得弹出。
    $sweepCount = [Math]::Min(10, $cards.Count)
    for ($k = 0; $k -lt $sweepCount; $k++) {
        $sr = $cards[$k].Current.BoundingRectangle
        Move-Cursor ([int]($sr.Left + $sr.Width / 2)) ([int]($sr.Top + $sr.Height / 2))
        Start-Sleep -Milliseconds 70
    }
    # 先把光标撤到窗口外缘空白带再等待:停留在末卡上会让它补足 1s,弹出属预期。
    Move-Cursor 8 8
    Start-Sleep -Milliseconds 1400
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (Find-UiaByAutomationId -Root $root -Id "peek-card") { throw "E23 FAIL: 快速掠过仍弹出预览卡" }
    $proc.Refresh()
    if ($proc.HasExited) { throw "E23 FAIL: 掠过后进程退出 ($($proc.ExitCode))" }
    Write-Host "[E23] 快速掠过 $sweepCount 卡无弹出" -ForegroundColor DarkCyan

    # ② 悬停首卡中心 1.4s -> peek-card 弹出(0.5s 时先断言未提前弹出)。
    $r0 = $cards[0].Current.BoundingRectangle
    $c0x = [int]($r0.Left + $r0.Width / 2)
    $c0y = [int]($r0.Top + $r0.Height / 2)
    # PointerEntered 只对真实移动消息生效:移入后做 1-2px 微动保持 hover 活跃。
    Move-Cursor $c0x $c0y
    Start-Sleep -Milliseconds 60
    Move-Cursor ($c0x + 2) $c0y
    Start-Sleep -Milliseconds 500
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $peek = Find-UiaByAutomationId -Root $root -Id "peek-card"
    if ($peek) { throw "E23 FAIL: 悬停 0.5s(<1s)预览卡不应出现" }
    Start-Sleep -Milliseconds 900
    Move-Cursor ($c0x + 3) ($c0y + 1)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $peek = Find-UiaByAutomationId -Root $root -Id "peek-card"
    if (-not $peek) { throw "E23 FAIL: 悬停 1.4s 后预览卡未弹出" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e23-1-peek-open.png"
    Write-Host "[E23] peek-card 已弹出" -ForegroundColor DarkCyan

    # ②.5 ✕ 手动关闭只静默当前卡:原卡停留不重弹,换卡立即恢复(会话级静默回归点)。
    $closeBtn = Find-UiaByName -Root $peek -Name "关闭预览"
    if (-not $closeBtn) { throw "E23 FAIL: 预览卡内未找到「关闭预览」按钮" }
    [void](Invoke-UiaElement -Element $closeBtn -What "预览卡关闭")
    Start-Sleep -Milliseconds 400
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (Find-UiaByAutomationId -Root $root -Id "peek-card") { throw "E23 FAIL: ✕ 后预览卡未收起" }
    # 光标仍停在原卡上微动 1.4s:不得重弹。
    Move-Cursor ($c0x + 4) ($c0y + 1)
    Start-Sleep -Milliseconds 700
    Move-Cursor ($c0x + 5) ($c0y + 2)
    Start-Sleep -Milliseconds 700
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (Find-UiaByAutomationId -Root $root -Id "peek-card") { throw "E23 FAIL: ✕ 后指针未离开原卡不得重弹" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e23-2-after-close.png"
    # 换到第二张卡悬停 1.4s:必须恢复弹出。
    $r1 = $cards[1].Current.BoundingRectangle
    $c1x = [int]($r1.Left + $r1.Width / 2)
    $c1y = [int]($r1.Top + $r1.Height / 2)
    Move-Cursor $c1x $c1y
    Start-Sleep -Milliseconds 60
    Move-Cursor ($c1x + 2) $c1y
    Start-Sleep -Milliseconds 1400
    Move-Cursor ($c1x + 3) ($c1y + 1)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (-not (Find-UiaByAutomationId -Root $root -Id "peek-card")) { throw "E23 FAIL: ✕ 后换卡悬停未恢复弹出" }
    Write-Host "[E23] ✕ 单卡静默 + 换卡恢复 已验证" -ForegroundColor DarkCyan
    # 撤到空白带让宽限收起,再重新悬停首卡取新预览引用(旧 $peek 已随 ✕ 销毁)。
    Move-Cursor 8 8
    Start-Sleep -Milliseconds 500
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (Find-UiaByAutomationId -Root $root -Id "peek-card") { throw "E23 FAIL: 移开空白带后预览卡未收起" }
    Move-Cursor $c0x $c0y
    Start-Sleep -Milliseconds 60
    Move-Cursor ($c0x + 2) $c0y
    Start-Sleep -Milliseconds 1400
    Move-Cursor ($c0x + 3) ($c0y + 1)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $peek = Find-UiaByAutomationId -Root $root -Id "peek-card"
    if (-not $peek) { throw "E23 FAIL: ✕ 后离开原卡再悬停未恢复弹出" }

    # ③ UIA Invoke 预览卡内「播放」-> 进入播放页(画质选择器出现,预览收起)。
    $playBtn = Find-UiaByName -Root $peek -Name "播放"
    if (-not $playBtn) { throw "E23 FAIL: 预览卡内未找到「播放」按钮" }
    [void](Invoke-UiaElement -Element $playBtn -What "预览卡播放")
    $inPlayer = $false
    for ($i = 0; $i -lt 20; $i++) {
        Start-Sleep -Milliseconds 500
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        if (Find-UiaByAutomationId -Root $root -Id "player-quality-selector") { $inPlayer = $true; break }
    }
    if (-not $inPlayer) { throw "E23 FAIL: 预览「播放」后未进入播放页" }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (Find-UiaByAutomationId -Root $root -Id "peek-card") { throw "E23 FAIL: 进入播放页后预览卡未收起" }
    Write-Host "[E23] 预览播放 -> 播放页 已验证" -ForegroundColor DarkCyan
    Write-Host "[E23] PASS: 悬停 1s 弹出 + ✕ 单卡静默换卡恢复 + UIA 播放进页 + 快速掠过不弹出无 panic" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:settingsSeeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    if ($stderrTask -and $stderrTask.Wait(5000)) {
        $text = $stderrTask.Result
        $text | Set-Content -Path $stderrLog -Encoding UTF8
        Write-Host "[E23] stderr -> $stderrLog" -ForegroundColor DarkCyan
    }
}
}

# ---------- 场景 E24：PiP/主位翻转不触发 surface 重建（native 换源重载回归） ----------
# 对应 P0-1: pages/player.rs::floating_surface_content 统一两态树形。
# 用播控条 [右下角小窗]/[还原主窗] 按钮确定性翻转 pip_pinned(滚动阈值路径受内容高度
# 影响不稳定),每轮断言按钮文案确实翻转,再校验 stderr 无
# "native surface detached: resume" / "native surface remount: 换源续播" 配对。
# 全屏/半窗的同类重载属 P1 架构项,不在本场景覆盖范围。
if (Should-RunScenario "E24") {
Write-Host "[E24] PiP/主位树形统一回归：UIA 按钮往返翻转，断言零 detach/remount" -ForegroundColor Cyan
$stderrLog = Join-Path $OutRoot "e24-stderr.log"
$proc = $null
$stderrSink = $null
$stderrTask = $null
$script:E24Fail = $false
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
    Start-Sleep -Milliseconds 5500 # native DASH 拉流 + 首帧预热(同 E12/E10)

    # 3 轮确定性往返:点 [右下角小窗] 进 PiP → 点 [还原主窗] 回主位。
    # 按钮文案随 is_pip 翻转(player.rs:608-625),可据此确认本次翻转真的发生。
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $pipBtn = Find-UiaByName -Root $root -Name "右下角小窗"
    if (-not $pipBtn) { throw "E24 FAIL: 未找到播控条 [右下角小窗] 按钮" }
    for ($cycle = 1; $cycle -le 3; $cycle++) {
        [void](Invoke-UiaElement -Element $pipBtn -What "进入小窗")
        Start-Sleep -Milliseconds 800
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $mainBtn = Find-UiaByName -Root $root -Name "还原主窗"
        if (-not $mainBtn) { throw "E24 FAIL: 第 $cycle 轮点击后未进入 PiP 态(按钮文案未翻转)" }
        if ($cycle -eq 1) { Capture-WindowFrame -Hwnd $hwnd -FileName "e24-1-pip.png" }
        [void](Invoke-UiaElement -Element $mainBtn -What "还原主窗")
        Start-Sleep -Milliseconds 800
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $pipBtn = Find-UiaByName -Root $root -Name "右下角小窗"
        if (-not $pipBtn) { throw "E24 FAIL: 第 $cycle 轮还原后未回到主位态" }
    }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e24-2-main.png"

    $proc.Refresh()
    if ($proc.HasExited) { throw "E24 FAIL: 进程提前退出 ($($proc.ExitCode))" }
    Write-Host "[E24] 3 轮 PiP/主位翻转已执行" -ForegroundColor DarkCyan
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask -and $stderrTask.Wait(5000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E24] stderr -> $stderrLog" -ForegroundColor DarkCyan
            if ($text -notmatch "native build_source ok" -or $text -notmatch "native media opened") {
                Write-Host "[E24] FAIL: native 播放基线锚点缺失，本次翻转未真正命中播放通路" -ForegroundColor Red
                $script:E24Fail = $true
            }
            $detachCount = ([regex]::Matches($text, [regex]::Escape("native surface detached: resume"))).Count
            $remountCount = ([regex]::Matches($text, [regex]::Escape("native surface remount: 换源续播"))).Count
            Write-Host "[E24] detach=$detachCount remount=$remountCount" -ForegroundColor DarkCyan
            if ($detachCount -gt 0 -or $remountCount -gt 0) {
                Write-Host "[E24] FAIL: PiP 翻转仍触发 surface 重建/换源重载" -ForegroundColor Red
                $script:E24Fail = $true
            }
        }
    } catch {}
}
if ($script:E24Fail) { throw "E24 FAIL: PiP/主位翻转触发重载(详见 $stderrLog)" }
Write-Host "[E24] PASS: 3 轮 PiP 翻转零 detach/remount，native 播放链未中断" -ForegroundColor Green
}

# ---------- 场景 E25：全屏进出不销毁 MPE（native 换源重载回归） ----------
# P1-1:全屏不再换根树,播放页组件留在 shell-root 原 keyed 位置,HUD 只是覆盖层。
# 判据:2 轮进/出全屏后 stderr 中 detach/remount 计数为 0。
if (Should-RunScenario "E25") {
Write-Host "[E25] 全屏进出零重载回归：UIA 按钮往返 2 轮，断言零 detach/remount" -ForegroundColor Cyan
$stderrLog = Join-Path $OutRoot "e25-stderr.log"
$proc = $null
$stderrSink = $null
$stderrTask = $null
$script:E25Fail = $false
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
    Start-Sleep -Milliseconds 5500

    for ($cycle = 1; $cycle -le 2; $cycle++) {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $fsBtn = Find-PlayerFullscreenButton -Root $root
        if (-not $fsBtn) { throw "E25 FAIL: 第 $cycle 轮未找到全屏按钮" }
        [void](Invoke-UiaElement -Element $fsBtn -What "进入全屏")
        Start-Sleep -Milliseconds 1200
        Assert-FullscreenBounds -Hwnd $hwnd -StepName "E25 第 $cycle 轮进入全屏"
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $exitBtn = Find-HudExitButton -Root $root
        if (-not $exitBtn) { throw "E25 FAIL: 第 $cycle 轮 HUD 未找到退出全屏按钮" }
        [void](Invoke-UiaElement -Element $exitBtn -What "退出全屏")
        Start-Sleep -Milliseconds 1200
    }
    $proc.Refresh()
    if ($proc.HasExited) { throw "E25 FAIL: 进程提前退出 ($($proc.ExitCode))" }
    Write-Host "[E25] 2 轮全屏进出已执行" -ForegroundColor DarkCyan
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask -and $stderrTask.Wait(5000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E25] stderr -> $stderrLog" -ForegroundColor DarkCyan
            if ($text -notmatch "native build_source ok" -or $text -notmatch "native media opened") {
                Write-Host "[E25] FAIL: native 播放基线锚点缺失" -ForegroundColor Red
                $script:E25Fail = $true
            }
            $detachCount = ([regex]::Matches($text, [regex]::Escape("native surface detached: resume"))).Count
            $remountCount = ([regex]::Matches($text, [regex]::Escape("native surface remount: 换源续播"))).Count
            Write-Host "[E25] detach=$detachCount remount=$remountCount" -ForegroundColor DarkCyan
            if ($detachCount -gt 0 -or $remountCount -gt 0) {
                Write-Host "[E25] FAIL: 全屏进出仍触发 surface 重建/换源重载" -ForegroundColor Red
                $script:E25Fail = $true
            }
        }
    } catch {}
}
if ($script:E25Fail) { throw "E25 FAIL: 全屏进出触发重载(详见 $stderrLog)" }
Write-Host "[E25] PASS: 2 轮全屏进出零 detach/remount" -ForegroundColor Green
}

# ---------- 场景 E26：半窗 <-> 主窗 不销毁 MPE（native 换源重载回归） ----------
# P1-2:半窗复用同一 PlayerPage + player:{bvid} key,窄宽度走移动档;展开只切布局。
# 判据:2 轮 [展开半窗->主窗->回半窗] 后 stderr 中 detach/remount 计数为 0。
if (Should-RunScenario "E26") {
Write-Host "[E26] 半窗/主窗零重载回归：UIA 展开半窗一次，断言零 detach/remount" -ForegroundColor Cyan
$script:E26Cycles = 1
$stderrLog = Join-Path $OutRoot "e26-stderr.log"
$settingsPath = Join-Path $dataDir "settings.json"
$settingsBak = "$settingsPath.e26-bak"
$script:e26Seeded = $false
$proc = $null
$stderrSink = $null
$stderrTask = $null
$script:E26Fail = $false
try {
    if (Test-Path $settingsPath) { Copy-Item -Path $settingsPath -Destination $settingsBak -Force }
    [System.IO.File]::WriteAllText(
        $settingsPath,
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"HalfPane","danmaku_enabled":true,"autoplay":true,"hardware_decode":true,"volume":60,"webp_hint_shown":true,"player_engine":"Native","window":{"x":60,"y":60,"width":1296,"height":808,"maximized":false}}'
    )
    $script:e26Seeded = $true

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables.Remove("QAQ_BX_VIDEO")
    $psi.EnvironmentVariables["QAQ_BX_NATIVE"] = "1"
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stderrSink = $proc.StandardError
    $stderrTask = $stderrSink.ReadToEndAsync()
    $hwnd = Wait-MainWindow $proc
    for ($i = 0; $i -lt 5; $i++) {
        if ([UiEdgeNative]::GetForegroundWindow() -eq $hwnd) { break }
        [UiEdgeNative]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero)
        [UiEdgeNative]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
        [void][UiEdgeNative]::SetForegroundWindow($hwnd)
        Start-Sleep -Milliseconds 200
    }
    Start-Sleep -Milliseconds 3500

    # 打开首页卡片进入半窗
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $condBtn = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    $buttons = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condBtn)
    $cardBtn = $null
    for ($i = 0; $i -lt $buttons.Count; $i++) {
        $br = $buttons[$i].Current.BoundingRectangle
        if (-not ([double]::IsInfinity($br.X) -or [double]::IsNaN($br.X)) -and
            $br.Top -ge 180 -and $br.Width -ge 120 -and $br.Height -ge 100) {
            $cardBtn = $buttons[$i]; break
        }
    }
    if (-not $cardBtn) { throw "E26 FAIL: 首页未找到可点击视频卡" }
    [void](Invoke-UiaElement -Element $cardBtn -What "打开半窗")
    Start-Sleep -Milliseconds 6500
    Capture-WindowFrame -Hwnd $hwnd -FileName "e26-0-halfpane.png"

    for ($cycle = 1; $cycle -le $script:E26Cycles; $cycle++) {
        # 展开半窗 -> 主窗播放页
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $expand = Find-UiaByAutomationId -Root $root -Id "half-pane-expand"
        if (-not $expand) {
            Capture-WindowFrame -Hwnd $hwnd -FileName "e26-fail-cycle-$cycle.png"
            throw "E26 FAIL: 第 $cycle 轮未找到 [half-pane-expand] 按钮(见 e26-fail-cycle-$cycle.png)"
        }
        [void](Invoke-UiaElement -Element $expand -What "展开半窗")
        Start-Sleep -Milliseconds 2000
        Capture-WindowFrame -Hwnd $hwnd -FileName "e26-1-expanded-cycle-$cycle.png"
    }
    $proc.Refresh()
    if ($proc.HasExited) { throw "E26 FAIL: 进程提前退出 ($($proc.ExitCode))" }
    Write-Host "[E26] 半窗→主窗展开已执行" -ForegroundColor DarkCyan
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    if ($script:e26Seeded) {
        if (Test-Path $settingsBak) { Copy-Item -Path $settingsBak -Destination $settingsPath -Force }
        else { Remove-Item -Path $settingsPath -Force -ErrorAction SilentlyContinue }
        Remove-Item -Path $settingsBak -Force -ErrorAction SilentlyContinue
    }
    try {
        if ($stderrSink) { $stderrSink.Close() }
        if ($stderrTask -and $stderrTask.Wait(5000)) {
            $text = $stderrTask.Result
            $text | Set-Content -Path $stderrLog -Encoding UTF8
            Write-Host "[E26] stderr -> $stderrLog" -ForegroundColor DarkCyan
            $detachCount = ([regex]::Matches($text, [regex]::Escape("native surface detached: resume"))).Count
            $remountCount = ([regex]::Matches($text, [regex]::Escape("native surface remount: 换源续播"))).Count
            Write-Host "[E26] detach=$detachCount remount=$remountCount" -ForegroundColor DarkCyan
            if ($detachCount -gt 0 -or $remountCount -gt 0) {
                Write-Host "[E26] FAIL: 半窗/主窗切换仍触发 surface 重建/换源重载" -ForegroundColor Red
                $script:E26Fail = $true
            }
        }
    } catch {}
}
if ($script:E26Fail) { throw "E26 FAIL: 半窗/主窗切换触发重载(详见 $stderrLog)" }
Write-Host "[E26] PASS: 半窗→主窗切换零 detach/remount" -ForegroundColor Green
}

# ---------- 场景 E27：列表页滚动自动续页 + 设置页一键清空缓存（真实滚轮 + UIA 点击） ----------
# 判据:① 热门页真实滚轮滚到底,「N 个视频」计数越过首屏 20;② 设置页 [settings-clear-caches]
# Invoke 后磁盘缓存目录 .bin 归零 + 「缓存已清理」InfoBar 出现;③ 白名单:settings.json 与
# 缓存目录内非 .bin 哨兵文件绝不被动。
if (Should-RunScenario "E27") {
Write-Host "[E27] 热门页滚轮自动续页 + 设置页一键清空缓存(白名单纪律)" -ForegroundColor Cyan
$proc = $null
try {
    $proc = Start-Process -FilePath $exe -WorkingDirectory (Split-Path $exe) -PassThru
    $hwnd = Wait-MainWindow $proc
    Start-Sleep -Milliseconds 3500
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (-not (Select-UiaNav -Root $root -Name "热门")) { throw "E27 FAIL: 导航栏未找到「热门」项" }

    # 页头副标题「N 个视频」计数解析(热门页专属文本)。
    function Get-PopularCount {
        param([System.Windows.Automation.AutomationElement]$Root)
        $condText = New-Object System.Windows.Automation.PropertyCondition(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        $texts = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condText)
        for ($i = 0; $i -lt $texts.Count; $i++) {
            if ($texts[$i].Current.Name -match "^(\d+) 个视频$") { return [int]$Matches[1] }
        }
        return -1
    }

    $base = -1
    for ($i = 0; $i -lt 40; $i++) {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $base = Get-PopularCount -Root $root
        if ($base -gt 0) { break }
        Start-Sleep -Milliseconds 500
    }
    if ($base -le 0) { throw "E27 FAIL: 热门页首屏计数未就绪" }
    Write-Host "[E27] 热门首屏 $base 个视频" -ForegroundColor DarkCyan

    # 真实滚轮滚向底部:每轮大幅下滚,自动续页应把计数推过首屏。
    $wr = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$wr)
    $cx = [int](($wr.Left + $wr.Right) / 2)
    $cy = [int](($wr.Top + $wr.Bottom) / 2)
    Move-Cursor $cx $cy
    $grown = $false
    for ($round = 1; $round -le 12; $round++) {
        Scroll-Fast -TotalDelta -3600
        Start-Sleep -Milliseconds 900
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $now = Get-PopularCount -Root $root
        Write-Host "[E27] 滚动第 $round 轮: $now 个视频" -ForegroundColor DarkCyan
        if ($now -gt $base) { $grown = $true; break }
    }
    if (-not $grown) { throw "E27 FAIL: 滚到底后热门计数未自动增长(自动续页失效)" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e27-0-popular-autopaged.png"
    # 等在飞封面下载落定,避免清缓存与写入赛跑。
    Start-Sleep -Seconds 4

    # 磁盘缓存目录应有真实 .bin(封面磁盘缓存落盘);放哨兵文件验证白名单。
    $imgCacheDir = Join-Path $dataDir "cache\images"
    if (-not (Test-Path $imgCacheDir)) { throw "E27 FAIL: 磁盘封面缓存目录未创建" }
    $binsBefore = @(Get-ChildItem -Path $imgCacheDir -Filter "*.bin" -File -ErrorAction SilentlyContinue)
    if ($binsBefore.Count -le 0) { throw "E27 FAIL: 浏览后磁盘缓存无 .bin 条目(磁盘缓存未生效)" }
    Write-Host "[E27] 磁盘缓存 $($binsBefore.Count) 条 .bin" -ForegroundColor DarkCyan
    $sentinel = Join-Path $imgCacheDir "e27-sentinel.txt"
    [System.IO.File]::WriteAllText($sentinel, "keep")
    $settingsPath = Join-Path $dataDir "settings.json"
    $settingsExisted = Test-Path $settingsPath
    $settingsHash = if ($settingsExisted) { (Get-FileHash $settingsPath -Algorithm SHA256).Hash } else { $null }

    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (-not (Select-UiaNav -Root $root -Name "设置")) { throw "E27 FAIL: 导航栏未找到「设置」项" }
    Start-Sleep -Milliseconds 800
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    $clearBtn = Find-UiaByAutomationId -Root $root -Id "settings-clear-caches"
    if (-not $clearBtn) { throw "E27 FAIL: 设置页未找到 [settings-clear-caches] 按钮" }
    # 在飞下载可能回填个别条目:最多点 3 轮直到 .bin 归零(幂等)。
    $cleared = $false
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        [void](Invoke-UiaElement -Element $clearBtn -What "清空缓存")
        Start-Sleep -Milliseconds 1200
        $binsAfter = @(Get-ChildItem -Path $imgCacheDir -Filter "*.bin" -File -ErrorAction SilentlyContinue)
        if ($binsAfter.Count -eq 0) { $cleared = $true; break }
    }
    if (-not $cleared) { throw "E27 FAIL: 一键清空后磁盘缓存仍有 .bin 条目" }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
    if (-not (Find-UiaByName -Root $root -Name "缓存已清理")) { throw "E27 FAIL: 清空后未出现「缓存已清理」结果提示" }
    if (-not (Test-Path $sentinel)) { throw "E27 FAIL: 白名单违规——缓存目录内非 .bin 文件被误删" }
    if ($settingsExisted) {
        if (-not (Test-Path $settingsPath)) { throw "E27 FAIL: 白名单违规——settings.json 被误删" }
        if ((Get-FileHash $settingsPath -Algorithm SHA256).Hash -ne $settingsHash) {
            throw "E27 FAIL: 白名单违规——settings.json 内容被改动"
        }
    }
    Remove-Item -Path $sentinel -Force -ErrorAction SilentlyContinue
    Capture-WindowFrame -Hwnd $hwnd -FileName "e27-1-caches-cleared.png"
    $proc.Refresh()
    if ($proc.HasExited) { throw "E27 FAIL: 进程提前退出 ($($proc.ExitCode))" }
    Write-Host "[E27] PASS: 滚轮自动续页 + 一键清空缓存 + 白名单(设置/哨兵)完好" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item -Path (Join-Path $dataDir "cache\images\e27-sentinel.txt") -Force -ErrorAction SilentlyContinue
}
}

# ---------- 场景 E28：首页横屏卡信息行顶边不变量（#10，标题长短不漂移） ----------
# 判据：首屏同一行内每张卡的标题块顶边、UP 主行顶边、▶/弹幕统计行顶边逐卡
# 完全一致（≤1 物理像素）；标题 1 行/2 行、UP 名长短都不得影响这些顶边。
if (Should-RunScenario "E28") {
Write-Host "[E28] 首页卡不变量：同一行卡片标题/UP/▶/弹幕顶边逐卡一致（与标题长度无关）" -ForegroundColor Cyan
# 隔离访客态数据目录，不碰用户真实凭据；预置默认设置压制首启 WebP 提示模态窗。
$e28Data = Join-Path $OutRoot "e28-data"
$script:e28DirReady = $false
$proc = $null
try {
    if (Test-Path $e28Data) { Remove-Item -Recurse -Force $e28Data }
    New-Item -ItemType Directory -Force -Path $e28Data | Out-Null
    $script:e28DirReady = $true
    # 字段与应用首启落盘格式一致：webp_hint_shown=true 压制首启「图片格式提示」
    # 模态窗(490x195，会把 Wait-MainWindow 拖到超时)；window=null 走默认尺寸，
    # 随后由 SetWindowPos 钉死位置尺寸。
    [System.IO.File]::WriteAllText(
        (Join-Path $e28Data "settings.json"),
        '{"quality":"Q1080","codec":"AvcFirst","home_open_mode":"FullPlayer","half_pane_width":480.0,"danmaku_enabled":true,"autoplay":false,"hardware_decode":true,"volume":80,"player_engine":"Native","webp_hint_shown":true,"portrait_mode":false,"prefer_official_cdn":true,"multi_thread_preload":true,"hover_preview":false,"reduce_animations":false,"window":null}'
    )

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.WorkingDirectory = (Split-Path $exe)
    $psi.UseShellExecute = $false
    $psi.EnvironmentVariables["QAQ_BX_DATA_DIR"] = $e28Data
    $proc = [System.Diagnostics.Process]::Start($psi)
    $hwnd = Wait-MainWindow $proc
    [void][UiEdgeNative]::SetWindowPos($hwnd, [IntPtr]::Zero, 60, 60, 1296, 808, [UiEdgeNative]::SWP_NOZORDER)

    # 等推荐流到货：命中盒=宽>150 高>120 的 Button（与 E23 同判据）。
    $cards = @()
    $btnCond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    $txtCond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    for ($i = 0; $i -lt 30; $i++) {
        Start-Sleep -Milliseconds 1000
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        $cards = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $btnCond) |
            Where-Object { $_.Current.BoundingRectangle.Width -gt 150 -and $_.Current.BoundingRectangle.Height -gt 120 })
        if ($cards.Count -ge 8) { break }
    }
    if ($cards.Count -lt 8) { throw "E28 FAIL: 首页可测视频卡不足 8 张($($cards.Count))" }
    Capture-WindowFrame -Hwnd $hwnd -FileName "e28-0-home.png"

    # 按行带分组（同一行卡 Top 相同），只取整卡完整落在客户区内的行，避免
    # 视口裁切把卡内文本的 UIA 矩形剪空造成误报。
    $wr = New-Object UiEdgeNative+RECT
    [void][UiEdgeNative]::GetWindowRect($hwnd, [ref]$wr)
    $visCards = @($cards | Where-Object {
        $b = $_.Current.BoundingRectangle
        $b.Top -ge $wr.Top -and $b.Bottom -le $wr.Bottom -and $b.Left -ge $wr.Left -and $b.Right -le $wr.Right
    })
    if ($visCards.Count -lt 3) { throw "E28 FAIL: 完整可见卡不足 3 张($($visCards.Count))" }
    $bands = $visCards | Group-Object { [Math]::Round($_.Current.BoundingRectangle.Top) } |
        Sort-Object { [double]$_.Name }

    $parseCard = {
        param($Card, $Cr, $Texts)
        $play = $null; $danmu = $null; $rest = @()
        foreach ($t in $Texts) {
            $name = $t.Current.Name
            $r = $t.Current.BoundingRectangle
            # UIA 空矩形(未渲染/视口裁切/离屏克隆)是 Empty/±∞：必须先滤掉再取 Top。
            if ([double]::IsNaN($r.Top) -or [double]::IsInfinity($r.Top) -or
                [double]::IsInfinity($r.Left) -or $r.Width -le 0 -or $r.Height -le 0) { continue }
            if ($t.Current.IsOffscreen) { continue }
            # 只认卡盒纵向带内的文本(卡顶下方 2px 起)，排除 peer 泄漏。
            if ($r.Top -lt $Cr.Top + 2 -or $r.Top -gt $Cr.Bottom) { continue }
            $top = [Math]::Round($r.Top, 1)
            if ($name -match '^▶ ') { $play = $top }
            elseif ($name -match '^弹幕 ') { $danmu = $top }
            elseif ($name -match '^\d{1,2}(:\d{2}){1,2}$') { }        # 时长徽标(封面盖层)
            elseif ($name -in @('封面加载中', '无法播放', '充电专稿')) { } # 占位/角标
            else { $rest += [pscustomobject]@{ Top = $top; Name = $name } }
        }
        if ($null -eq $play -or $null -eq $danmu -or $rest.Count -lt 2) { return $null }
        $rest = @($rest | Sort-Object Top)
        # rest[0]=标题块（定高槽，顶边=槽顶），rest[-1]=UP 主行。
        [pscustomobject]@{ Title = $rest[0].Top; Author = $rest[-1].Top; Play = $play; Danmu = $danmu }
    }

    # 自上而下扫描行带：被视口底缘裁切的行整行解析为空(属采样问题而非布局缺陷)，
    # 取第一个含 ≥3 张完整卡的行做严格同位断言。
    $report = @()
    $bandTouched = 0
    foreach ($band in $bands) {
        $bandTouched++
        $candidate = @()
        foreach ($card in @($band.Group)) {
            $cr = $card.Current.BoundingRectangle
            $texts = @($card.FindAll([System.Windows.Automation.TreeScope]::Descendants, $txtCond))
            $parsed = & $parseCard $card $cr $texts
            if ($parsed) { $candidate += $parsed }
        }
        Write-Host ("[E28] band top={0}: {1}/{2} 卡要素完整" -f $band.Name, $candidate.Count, $band.Count) -ForegroundColor DarkCyan
        if ($candidate.Count -ge 3) { $report = $candidate; break }
    }
    if ($report.Count -lt 3) {
        throw "E28 FAIL: 扫描 $($bandTouched) 个行带仍无 ≥3 张要素完整卡(视口/流未就绪?)"
    }
    Write-Host "[E28] 采样行带 top=$($bands[$bandTouched-1].Name)：完整卡 $($report.Count) 张" -ForegroundColor DarkCyan

    foreach ($key in @('Title', 'Author', 'Play', 'Danmu')) {
        $tops = @($report | ForEach-Object { $_.$key })
        $spread = ($tops | Measure-Object -Maximum).Maximum - ($tops | Measure-Object -Minimum).Minimum
        Write-Host ("[E28] {0} tops={1} spread={2}" -f $key, ($tops -join ','), $spread) -ForegroundColor DarkCyan
        if ($spread -gt 1.0) {
            throw "E28 FAIL: $key 行顶边跨卡漂移 $spread px（定高槽不变量被破坏，标题长短在影响下方行位）"
        }
    }
    # 行序 sanity：标题槽顶 < UP 顶 < 统计顶，且统计行两要素同行。
    foreach ($r in $report) {
        if (-not ($r.Title -lt $r.Author -and $r.Author -lt $r.Play)) {
            throw "E28 FAIL: 行序异常 title=$($r.Title) author=$($r.Author) play=$($r.Play)"
        }
        if ([Math]::Abs($r.Play - $r.Danmu) -gt 1.0) { throw "E28 FAIL: ▶/弹幕 不同行" }
    }
    Write-Host "[E28] PASS: $($report.Count) 卡标题/UP/▶/弹幕顶边逐卡一致（±1px）" -ForegroundColor Green
} finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 400
    if ($script:e28DirReady) { Remove-Item -Recurse -Force $e28Data -ErrorAction SilentlyContinue }
}
}
