param([int]$WaitSecs = 50, [string]$Profile = "debug", [switch]$NoVideo)
$exe = "target\$Profile\qaq-bx-ui.exe"
if (-not (Test-Path $exe)) { Write-Error "missing $exe"; exit 1 }
if (-not $NoVideo) { $env:QAQ_BX_VIDEO = "BV1GJ411x7h7" }
$errLog = Join-Path $env:TEMP "qaq-bx-$Profile-stderr.log"
$proc = Start-Process -FilePath $exe -WorkingDirectory (Get-Location) -PassThru `
    -RedirectStandardError $errLog
Start-Sleep -Seconds $WaitSecs
$samples = @()
foreach ($i in 1..5) {
    $proc.Refresh()
    if ($proc.HasExited) { Write-Output "EXITED code=$($proc.ExitCode)"; break }
    $samples += [math]::Round($proc.WorkingSet64 / 1MB, 1)
    Start-Sleep -Seconds 3
}
$private = [math]::Round($proc.PrivateMemorySize64 / 1MB, 1)
Write-Output ("PROFILE={0} SAMPLES_MB={1} PRIVATE_MB={2}" -f $Profile, ($samples -join ","), $private)
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
Write-Output "--- engine stderr tail ---"
Get-Content $errLog -Tail 25 -ErrorAction SilentlyContinue
