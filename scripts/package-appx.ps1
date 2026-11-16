<#
.SYNOPSIS
    构建并打包 biliplay AppX 安装包（支持 debug+自签名 / release+自签名 / release 无签名）。
.EXAMPLE
    # 1. debug + 自签名 -> package/debug/biliplayRS-dev_0.0.0.0_x64.appx
    .\scripts\package-appx.ps1 -Profile debug -Sign

    # 2. release + 自签名 -> package/release/biliplayRS_0.0.0.0_x64.appx
    .\scripts\package-appx.ps1 -Profile release -Sign

    # 3. release（无签名）-> package/release/biliplayRS_0.0.0.0_x64.appx
    .\scripts\package-appx.ps1 -Profile release
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("debug", "release")]
    [string]$Profile,

    [switch]$Sign,

    [string]$Publisher = "CN=BiliplayDev",

    [string]$CertPassword = "biliplay-dev"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

function Find-SdkTool {
    param([string]$ToolName)
    $cmd = Get-Command $ToolName -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $kitsRoot = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (Test-Path $kitsRoot) {
        $found = Get-ChildItem -Path $kitsRoot -Recurse -Filter $ToolName -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "\\x64\\" } |
            Sort-Object FullName -Descending |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    throw "未找到 Windows SDK 工具: $ToolName，请安装 Windows 10/11 SDK。"
}

function Get-WorkspaceVersion4Part {
    $cargoToml = Get-Content (Join-Path $RepoRoot "Cargo.toml") -Raw
    if ($cargoToml -match '(?ms)\[workspace\.package\].*?version\s*=\s*"(\d+\.\d+\.\d+)"') {
        return "$($Matches[1]).0"
    }
    throw "无法从 Cargo.toml 解析 [workspace.package].version"
}

function Ensure-SelfSignedCert {
    param(
        [string]$CertDir,
        [string]$Subject,
        [string]$Password
    )
    New-Item -ItemType Directory -Force -Path $CertDir | Out-Null
    $pfxPath = Join-Path $CertDir "biliplay-dev.pfx"
    $cerPath = Join-Path $CertDir "biliplay-dev.cer"
    if ((Test-Path $pfxPath) -and (Test-Path $cerPath)) {
        return @{ Pfx = $pfxPath; Cer = $cerPath }
    }

    Write-Host "[cert] 生成自签名开发证书 ($Subject)..." -ForegroundColor Cyan
    $cert = New-SelfSignedCertificate `
        -Type Custom `
        -Subject $Subject `
        -KeyUsage DigitalSignature `
        -FriendlyName "biliplayRS Dev Self-Signed" `
        -CertStoreLocation "Cert:\CurrentUser\My" `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")

    $secPwd = ConvertTo-SecureString -String $Password -Force -AsPlainText
    Export-PfxCertificate -Cert $cert -FilePath $pfxPath -Password $secPwd | Out-Null
    Export-Certificate -Cert $cert -FilePath $cerPath | Out-Null
    Write-Host "[cert] 已生成: $pfxPath (安装测试时需将 $cerPath 导入 LocalMachine\TrustedPeople)" -ForegroundColor Yellow
    return @{ Pfx = $pfxPath; Cer = $cerPath }
}

$version4 = Get-WorkspaceVersion4Part
$isRelease = ($Profile -eq "release")
$packageName = if ($isRelease) { "biliplayRS" } else { "biliplayRS-dev" }
$displayName = if ($isRelease) { "biliplayRS" } else { "biliplayRS (Dev)" }

$makeAppx = Find-SdkTool "makeappx.exe"
if ($Sign) {
    $signTool = Find-SdkTool "signtool.exe"
}

Write-Host "[build] cargo build $(if ($isRelease) { '--release ' })-p bili-ui (v$version4 / $packageName)" -ForegroundColor Cyan
Push-Location $RepoRoot
try {
    if ($isRelease) {
        & cargo build --release -p bili-ui
    } else {
        & cargo build -p bili-ui
    }
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build 失败 (exit=$LASTEXITCODE)"
    }
} finally {
    Pop-Location
}

$targetProfileDir = Join-Path $RepoRoot "target\$Profile"
$stagingDir = Join-Path $RepoRoot "target\appx-staging\$Profile"
$outDir = Join-Path $RepoRoot "package\$Profile"
$outAppx = Join-Path $outDir "${packageName}_${version4}_x64.appx"

if (Test-Path $stagingDir) {
    Remove-Item -Recurse -Force $stagingDir
}
New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# 1. 复制可执行文件与 windows-reactor-setup 部署的运行时文件/子目录
$exePath = Join-Path $targetProfileDir "bili-ui.exe"
if (-not (Test-Path $exePath)) {
    throw "未找到编译产物: $exePath"
}
Copy-Item $exePath $stagingDir

$excludedDirs = @("build", "deps", "examples", "incremental", ".fingerprint", "data")
Get-ChildItem -Path $targetProfileDir -Directory | Where-Object {
    $excludedDirs -notcontains $_.Name
} | ForEach-Object {
    Copy-Item -Recurse -Force $_.FullName (Join-Path $stagingDir $_.Name)
}

$runtimeExts = @(".dll", ".winmd", ".pri", ".xbf", ".mui", ".json")
Get-ChildItem -Path $targetProfileDir -File | Where-Object {
    ($runtimeExts -contains $_.Extension.ToLowerInvariant()) -and ($_.Name -notlike "biliplay*")
} | ForEach-Object {
    Copy-Item -Force $_.FullName (Join-Path $stagingDir $_.Name)
}

# 2. 确保 libmpv-2.dll 就位
$mpvDst = Join-Path $stagingDir "libmpv-2.dll"
if (-not (Test-Path $mpvDst)) {
    $mpvCandidates = @(
        (Join-Path $RepoRoot ".qaqh\mpv\mpv-dev\libmpv-2.dll"),
        (Join-Path $RepoRoot "target\debug\libmpv-2.dll")
    )
    $mpvSrc = $mpvCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if ($mpvSrc) {
        Copy-Item -Force $mpvSrc $mpvDst
    } else {
        Write-Warning "未找到 libmpv-2.dll，打包后的应用将无法播放视频。请将其放入 $targetProfileDir 或 .qaqh\mpv\mpv-dev\。"
    }
}

# 3. 复制 Assets 与渲染 AppxManifest.xml
$assetsSrc = Join-Path $RepoRoot "packaging\appx\Assets"
Copy-Item -Recurse -Force $assetsSrc (Join-Path $stagingDir "Assets")

$manifestTpl = Get-Content (Join-Path $RepoRoot "packaging\appx\AppxManifest.template.xml") -Raw
$manifestXml = $manifestTpl `
    -replace '\{\{PACKAGE_NAME\}\}', $packageName `
    -replace '\{\{DISPLAY_NAME\}\}', $displayName `
    -replace '\{\{PUBLISHER\}\}', $Publisher `
    -replace '\{\{VERSION\}\}', $version4
Set-Content -Path (Join-Path $stagingDir "AppxManifest.xml") -Value $manifestXml -Encoding UTF8

# 4. MakeAppx 打包到 /package/<profile>/
Write-Host "[pack] $makeAppx -> $outAppx" -ForegroundColor Cyan
& $makeAppx pack /o /d $stagingDir /p $outAppx
if ($LASTEXITCODE -ne 0) {
    throw "makeappx 打包失败 (exit=$LASTEXITCODE)"
}

# 5. 可选：自签名（并将公钥证书 .cer 同步复制到输出目录与 .appx 放在一起）
if ($Sign) {
    $certDir = Join-Path $RepoRoot "packaging\certs"
    $certFiles = Ensure-SelfSignedCert -CertDir $certDir -Subject $Publisher -Password $CertPassword
    Write-Host "[sign] $signTool -> $outAppx" -ForegroundColor Cyan
    & $signTool sign /fd SHA256 /f $certFiles.Pfx /p $CertPassword $outAppx
    if ($LASTEXITCODE -ne 0) {
        throw "signtool 签名失败 (exit=$LASTEXITCODE)"
    }
    $outCer = Join-Path $outDir "${packageName}_${version4}_x64.cer"
    Copy-Item -Force $certFiles.Cer $outCer
    Write-Host "[cert] 同步输出公钥证书: $outCer" -ForegroundColor Green
}

Write-Host "[done] 输出包: $outAppx" -ForegroundColor Green
