# Runs elevated. Removes the file-backed test queues and their ports.
$ErrorActionPreference = 'Continue'
$dir = $PSScriptRoot
foreach ($n in 'DocuView Test PS3', 'DocuView Test PCL6', 'DocuView Test PDF') { Remove-Printer -Name $n -ErrorAction SilentlyContinue }
Start-Sleep 2
foreach ($f in 'out-ps3.prn', 'out-pcl6.prn', 'out-pdf.pdf') { Remove-PrinterPort -Name (Join-Path $dir $f) -ErrorAction SilentlyContinue }
Get-Printer | Where-Object Name -like 'DocuView Test*' | Select-Object Name | Out-String | Set-Content (Join-Path $dir 'remove-test-printers.log')
Add-Content (Join-Path $dir 'remove-test-printers.log') 'done'
