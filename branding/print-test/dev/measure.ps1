# Time one print job end to end: start the command, watch the queue every quarter second
# (status, pages spooled, spool size) and the driver's output file, and keep the output as out-<label>.prn.
param(
  [Parameter(Mandatory)] [string] $Printer,
  [Parameter(Mandatory)] [string] $Label,
  [Parameter(Mandatory)] [string] $Command,     # a PowerShell command line to run (the thing that prints)
  [int] $TimeoutSec = 900
)
$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$port = (Get-Printer -Name $Printer).PortName
if (Test-Path $port) { Remove-Item $port -Force }
$log = Join-Path $dir "timeline-$Label.csv"
"t,status,pages,total,spoolBytes,prnBytes" | Set-Content $log

$sw = [Diagnostics.Stopwatch]::StartNew()
$proc = Start-Process powershell -ArgumentList '-NoProfile', '-Command', $Command -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $dir "cmd-$Label.out") -RedirectStandardError (Join-Path $dir "cmd-$Label.err")
$seen = $false; $firstPage = $null; $lastPages = -1; $jobGone = $null
while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
  $job = Get-PrintJob -PrinterName $Printer -ErrorAction SilentlyContinue | Select-Object -First 1
  $prn = if (Test-Path $port) { (Get-Item $port).Length } else { 0 }
  if ($job) {
    $seen = $true
    if ($job.PagesPrinted -ne $lastPages) {
      "{0:F2},{1},{2},{3},{4},{5}" -f $sw.Elapsed.TotalSeconds, $job.JobStatus, $job.PagesPrinted, $job.TotalPages, $job.Size, $prn | Add-Content $log
      if ($null -eq $firstPage -and $job.PagesPrinted -ge 1) { $firstPage = $sw.Elapsed.TotalSeconds }
      $lastPages = $job.PagesPrinted
    }
  } elseif ($seen -and $null -eq $jobGone) {
    $jobGone = $sw.Elapsed.TotalSeconds
    "{0:F2},gone,,,,{1}" -f $jobGone, $prn | Add-Content $log
  }
  if ($proc.HasExited -and ($jobGone -or -not $seen -and $sw.Elapsed.TotalSeconds -gt 20)) { break }
  if ($jobGone -and $proc.HasExited) { break }
  Start-Sleep -Milliseconds 250
}
$prnFinal = if (Test-Path $port) { (Get-Item $port).Length } else { 0 }
# The port keeps writing briefly after the job leaves the queue.
for ($i = 0; $i -lt 20; $i++) { Start-Sleep -Milliseconds 500; $n = if (Test-Path $port) { (Get-Item $port).Length } else { 0 }; if ($n -eq $prnFinal) { break }; $prnFinal = $n }
if (Test-Path $port) { Copy-Item $port (Join-Path $dir "out-$Label.prn") -Force }
[pscustomobject]@{
  label        = $Label
  commandExit  = $proc.ExitCode
  commandSecs  = [math]::Round($sw.Elapsed.TotalSeconds, 1)
  firstPageSec = $firstPage
  jobDoneSec   = $jobGone
  outputBytes  = $prnFinal
} | Format-List
"--- timeline"; Get-Content $log
"--- command output"; Get-Content (Join-Path $dir "cmd-$Label.out") -ErrorAction SilentlyContinue | Select-Object -First 15
Get-Content (Join-Path $dir "cmd-$Label.err") -ErrorAction SilentlyContinue | Select-Object -First 15
