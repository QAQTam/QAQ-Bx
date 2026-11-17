param(
    [switch]$Record,
    [switch]$SkipBuild,
    [switch]$AutoExit,
    [int]$MaxWaitSec = 40
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$debugDir = Join-Path $repoRoot 'target\debug'
$exePath = Join-Path $debugDir 'qaq-bx-ui.exe'
$crashLog = Join-Path $debugDir 'data\crash.log'

if (-not $SkipBuild) {
    Write-Host '[ui-demo] Building qaq-bx-ui...'
    Push-Location $repoRoot
    try {
        cargo build -p qaq-bx-ui
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

if (-not (Test-Path $exePath)) {
    throw "Executable not found: $exePath"
}

$crashBefore = if (Test-Path $crashLog) { (Get-Item $crashLog).LastWriteTimeUtc } else { $null }

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class DemoStageWin32 {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct MONITORINFO {
        public uint cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
    }

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern bool GetClientRect(IntPtr hWnd, out RECT lpRect);

    [DllImport("user32.dll")]
    public static extern IntPtr MonitorFromWindow(IntPtr hWnd, uint dwFlags);

    [DllImport("user32.dll")]
    public static extern bool GetMonitorInfoW(IntPtr hMonitor, ref MONITORINFO lpmi);
}
"@

function Resolve-FfmpegPath {
    $cmd = Get-Command ffmpeg -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Links\ffmpeg.exe'),
        'C:\ffmpeg\bin\ffmpeg.exe'
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path $c)) { return $c }
    }
    throw 'ffmpeg.exe not found in PATH or WinGet Links.'
}

$shouldExit = $Record -or $AutoExit
$env:QAQ_BX_DEMO = '1'
if ($shouldExit) {
    $env:QAQ_BX_DEMO_EXIT = '1'
} else {
    Remove-Item Env:QAQ_BX_DEMO_EXIT -ErrorAction SilentlyContinue
}

$argList = @('--demo')
if ($shouldExit) {
    $argList += '--demo-exit'
}

Write-Host "[ui-demo] Launching self-driving demo ($($argList -join ' '))..."
$proc = Start-Process -FilePath $exePath -ArgumentList $argList -WorkingDirectory $debugDir -PassThru

$hwnd = [IntPtr]::Zero
$deadline = (Get-Date).AddSeconds(12)
while ((Get-Date) -lt $deadline -and -not $proc.HasExited) {
    $proc.Refresh()
    if ($proc.MainWindowHandle -ne [IntPtr]::Zero) {
        $cr = New-Object DemoStageWin32+RECT
        if ([DemoStageWin32]::GetClientRect($proc.MainWindowHandle, [ref]$cr) -and ($cr.Right - $cr.Left) -ge 400) {
            $hwnd = $proc.MainWindowHandle
            break
        }
    }
    Start-Sleep -Milliseconds 100
}

if (-not $Record) {
    if ($shouldExit) {
        $null = $proc.WaitForExit($MaxWaitSec * 1000)
        if (-not $proc.HasExited) {
            $proc.Kill()
            throw 'Demo process did not exit within timeout.'
        }
        Write-Host "[ui-demo] Self-driving demo completed cleanly (ExitCode=$($proc.ExitCode))."
    } else {
        Write-Host '[ui-demo] Interactive demo running cleanly (no mouse automation).'
    }
} else {
    if ($hwnd -eq [IntPtr]::Zero) {
        if (-not $proc.HasExited) { $proc.Kill() }
        throw 'Failed to acquire main WinUI HWND for stage recording.'
    }
    # 等待 demo 首帧居中落地（t <= 1.2s 内仍保持 1280x800 初始宽度）
    Start-Sleep -Milliseconds 450
    $wr = New-Object DemoStageWin32+RECT
    [void][DemoStageWin32]::GetWindowRect($hwnd, [ref]$wr)
    $capX = [Math]::Max(0, $wr.Left)
    $capY = [Math]::Max(0, $wr.Top)
    $rawW = $wr.Right - $wr.Left
    $rawH = $wr.Bottom - $wr.Top
    $capW = $rawW - ($rawW % 2)
    $capH = $rawH - ($rawH % 2)
    if ($capW -lt 640 -or $capH -lt 480) {
        $capW = 1280
        $capH = 800
    }

    $outDir = Join-Path $repoRoot 'target\ui-demo'
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    $rawMp4 = Join-Path $outDir 'promo.mp4'
    $promoAssetMp4 = Join-Path $repoRoot 'promo-video\assets\raw-demo.mp4'
    $ffmpeg = Resolve-FfmpegPath

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $ffmpeg
    $psi.Arguments = "-y -f gdigrab -framerate 60 -offset_x $capX -offset_y $capY -video_size ${capW}x${capH} -draw_mouse 0 -i desktop -c:v libx264 -preset fast -crf 16 -pix_fmt yuv420p -movflags +faststart `"$rawMp4`""
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true

    Write-Host "[ui-demo] Recording fixed stage ${capW}x${capH} at ($capX,$capY) via ffmpeg..."
    $ffProc = [System.Diagnostics.Process]::Start($psi)
    $stderrTask = $ffProc.StandardError.ReadToEndAsync()

    $exited = $proc.WaitForExit($MaxWaitSec * 1000)
    if (-not $exited) {
        $proc.Kill()
    }

    try {
        $ffProc.StandardInput.WriteLine('q')
        $ffProc.StandardInput.Flush()
    } catch {}
    if (-not $ffProc.WaitForExit(6000)) {
        $ffProc.Kill()
    }
    $ffErr = $stderrTask.Result

    if (-not (Test-Path $rawMp4)) {
        throw "ffmpeg recording did not produce $rawMp4. stderr:`n$ffErr"
    }
    New-Item -ItemType Directory -Path (Split-Path -Parent $promoAssetMp4) -Force | Out-Null
    Copy-Item -Path $rawMp4 -Destination $promoAssetMp4 -Force
    $sizeMb = [Math]::Round((Get-Item $rawMp4).Length / 1MB, 2)
    Write-Host "[ui-demo] Recorded ${sizeMb} MB -> $rawMp4 (synced to $promoAssetMp4)"
}

if (Test-Path $crashLog) {
    $crashAfter = (Get-Item $crashLog).LastWriteTimeUtc
    if (-not $crashBefore -or $crashAfter -gt $crashBefore) {
        throw "crash.log updated during demo run: $crashLog"
    }
}
