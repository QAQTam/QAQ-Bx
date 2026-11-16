<#
.SYNOPSIS
    biliplayRS 极端场景走查：访客态（无本地凭据）与快速连续切视频。
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
Add-Type -ReferencedAssemblies System.Drawing @"
using System;
using System.Runtime.InteropServices;

public static class UiEdgeNative {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }

    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, int dx, int dy, int dwData, UIntPtr dwExtraInfo);
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP = 0x0004;
    public const uint MOUSEEVENTF_WHEEL = 0x0800;
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

function Scroll-Fast {
    param([int]$TotalDelta)
    for ($i = 0; $i -lt 12; $i++) {
        [UiEdgeNative]::mouse_event([UiEdgeNative]::MOUSEEVENTF_WHEEL, 0, 0, [int]($TotalDelta / 12), [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 22
    }
}

if (-not $SkipBuild) {
    Write-Host "[build] cargo build -p bili-ui" -ForegroundColor Cyan
    Push-Location $RepoRoot
    try {
        & cargo build -p bili-ui
        if ($LASTEXITCODE -ne 0) { throw "cargo build 失败" }
    } finally { Pop-Location }
}

$exe = Join-Path $RepoRoot "target\debug\bili-ui.exe"
if (-not (Test-Path $exe)) { throw "未找到 $exe" }
$dataDir = Join-Path $RepoRoot "target\debug\data"
$credPath = Join-Path $dataDir "credentials.json"
$credBackup = "$credPath.edge-bak"

function Wait-MainWindow {
    param([System.Diagnostics.Process]$Proc)
    $hwnd = [IntPtr]::Zero
    for ($i = 0; $i -lt 60; $i++) {
        $Proc.Refresh()
        if ($Proc.HasExited) { throw "bili-ui.exe 提前退出 ($($Proc.ExitCode))" }
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

Write-Host "[done] 极端场景走查完成，截图位于 $FramesDir" -ForegroundColor Green
