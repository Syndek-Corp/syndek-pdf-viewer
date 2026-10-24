# Runs elevated. Adds two local test queues that use the office copiers' own drivers but write
# their output to files, so a job can be measured without a copier. Remove with remove-test-printers.ps1.
$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$log = Join-Path $dir 'add-test-printers.log'
try {
  $queues = @(
    @{ Name = 'DocuView Test PS3';  Driver = 'TOSHIBA Universal PS3';       File = Join-Path $dir 'out-ps3.prn' },
    @{ Name = 'DocuView Test PCL6'; Driver = 'TOSHIBA Universal Printer 2'; File = Join-Path $dir 'out-pcl6.prn' }
  )
  foreach ($q in $queues) {
    if (-not (Get-PrinterPort -Name $q.File -ErrorAction SilentlyContinue)) { Add-PrinterPort -Name $q.File }
    if (Get-Printer -Name $q.Name -ErrorAction SilentlyContinue) { Remove-Printer -Name $q.Name }
    Add-Printer -Name $q.Name -DriverName $q.Driver -PortName $q.File
    # Start sending to the port only after the whole job is spooled, so the .prn is complete when the job leaves the queue.
    Set-Printer -Name $q.Name -RenderingMode SSR
  }
  # SYSTEM writes the .prn files; make sure the folder allows it.
  icacls $dir /grant 'SYSTEM:(OI)(CI)F' | Out-Null
  Get-Printer | Where-Object Name -like 'DocuView Test*' | Select-Object Name, DriverName, PortName, RenderingMode | Format-Table -AutoSize | Out-String | Set-Content $log -Encoding utf8
  Add-Content $log 'OK'
} catch {
  "ERROR: $($_ | Out-String)" | Set-Content $log -Encoding utf8
}
