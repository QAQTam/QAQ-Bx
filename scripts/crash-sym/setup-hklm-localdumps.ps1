# Requires elevation. Configures WER LocalDumps for the packaged app qaq-bx-ui.exe.
# HKCU LocalDumps is ignored for packaged (MoAppCrash) apps - verified empirically
# (3 crashes, EtwNonCollectReason=1, zero dumps). See docs/dev-notes/crash-0x359595-xamlroot.md §5.
$key = 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\qaq-bx-ui.exe'
New-Item -Path $key -Force | Out-Null
New-ItemProperty -Path $key -Name DumpFolder -PropertyType ExpandString -Value 'D:\QAQ-Bx\data\crash-dumps' -Force | Out-Null
New-ItemProperty -Path $key -Name DumpType -PropertyType DWord -Value 2 -Force | Out-Null
New-ItemProperty -Path $key -Name DumpCount -PropertyType DWord -Value 10 -Force | Out-Null
Get-ItemProperty -Path $key | Select-Object DumpFolder, DumpType, DumpCount | Format-List
