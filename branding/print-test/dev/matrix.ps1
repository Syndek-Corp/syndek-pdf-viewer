# Automated print matrix: every paper size / colour mode / driver through the GDI backend, with
# the sheet dump on, checked for sheet pixel size, depth, bands, output size and time.
param([string] $Cli = 'C:\Program Files\DocuView\pdfcraft-cli.exe')
$ErrorActionPreference = 'Stop'
$d = $PSScriptRoot
Set-Location $d
$results = @()

function Run-Case {
  param([string] $Name, [string] $Printer, [string] $StepsJson, [hashtable] $Expect)
  $dump = Join-Path $d "mx-$Name"
  if (Test-Path $dump) { Remove-Item -Recurse -Force $dump }
  New-Item -ItemType Directory -Force $dump | Out-Null
  $steps = Join-Path $d "mx-$Name.json"
  Set-Content $steps $StepsJson -Encoding utf8
  $port = (Get-Printer -Name $Printer).PortName
  if (Test-Path $port) { Remove-Item $port -Force }
  $env:DOCUVIEW_PRINT_DUMP = $dump
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $out = & $Cli run --script "mx-$Name.json" --root $d 2>&1 | Out-String
  $secs = [math]::Round($sw.Elapsed.TotalSeconds, 1)
  Remove-Item Env:DOCUVIEW_PRINT_DUMP
  # wait for the port file to settle
  $last = -1; for ($i = 0; $i -lt 40; $i++) { Start-Sleep -Milliseconds 500; $n = if (Test-Path $port) { (Get-Item $port).Length } else { 0 }; if ($n -eq $last -and $n -gt 0) { break }; $last = $n }
  $mb = if (Test-Path $port) { [math]::Round((Get-Item $port).Length / 1MB, 2) } else { 0 }
  $files = Get-ChildItem $dump | Sort-Object Name
  $sheets = @{}
  foreach ($f in $files) {
    if ($f.Name -match '^(sheet-\d+)(?:-y(\d+))?\.(mono|gray|rgb|jpeg)\.') {
      $key = $Matches[1]; $depth = $Matches[3]
      $size = python -c "from PIL import Image; im=Image.open(r'$($f.FullName)'); print(im.width, im.height)"
      $w, $h = $size -split ' '
      if (-not $sheets.ContainsKey($key)) { $sheets[$key] = @{ depth = $depth; w = [int]$w; rows = 0; bands = 0 } }
      if ($Matches[2]) { $sheets[$key].rows += [int]$h; $sheets[$key].bands++ } else { $sheets[$key].rows = [int]$h }
    }
  }
  $fails = @()
  if ($out -match 'pdfcraft-cli:|"error"|failed') { $fails += "error: $(($out -split "`n" | Select-String 'pdfcraft-cli:|"error"|failed' | Select-Object -First 1))" }
  foreach ($k in $Expect.Keys) {
    if ($k -eq 'maxMB') { continue }
    $e = $Expect[$k]
    if (-not $sheets.ContainsKey($k)) { $fails += "$k missing"; continue }
    $s = $sheets[$k]
    if ($e.w -and $s.w -ne $e.w) { $fails += "$k width $($s.w) != $($e.w)" }
    if ($e.h -and $s.rows -ne $e.h) { $fails += "$k height $($s.rows) != $($e.h)" }
    if ($e.depth -and $s.depth -ne $e.depth) { $fails += "$k depth $($s.depth) != $($e.depth)" }
    if ($e.bands -and $s.bands -ne $e.bands) { $fails += "$k bands $($s.bands) != $($e.bands)" }
  }
  if ($Expect.ContainsKey('maxMB') -and $mb -gt $Expect.maxMB) { $fails += "output $mb MB > $($Expect.maxMB)" }
  $script:results += [pscustomobject]@{ case = $Name; secs = $secs; outMB = $mb; sheets = ($sheets.Keys | Sort-Object | ForEach-Object { "$($_):$($sheets[$_].depth) $($sheets[$_].w)x$($sheets[$_].rows)" + $(if ($sheets[$_].bands) { "/$($sheets[$_].bands)b" } else { '' }) }) -join ' '; result = $(if ($fails) { 'FAIL ' + ($fails -join '; ') } else { 'ok' }) }
}

$open = '{ "tool": "doc_open", "args": { "path": "pack.pdf" } }'
foreach ($drv in @(@('ps3', 'DocuView Test PS3', 'jpeg'), @('pcl6', 'DocuView Test PCL6', 'rgb'))) {
  $q = $drv[1]; $colour = $drv[2]; $tag = $drv[0]
  Run-Case "letter-fit-$tag" $q "[ $open, { `"tool`": `"doc_print`", `"args`": { `"doc`": 1, `"printer`": `"$q`", `"pages`": `"1-4, 7-9`" } } ]" @{
    'sheet-1' = @{ w = 5100; h = 6600; depth = 'mono' }; 'sheet-2' = @{ w = 5100; h = 6600; depth = 'mono' }; 'sheet-3' = @{ w = 5100; h = 6600; depth = $colour }
    'sheet-4' = @{ w = 6600; h = 5100; depth = 'mono' }; 'sheet-5' = @{ w = 6600; h = 5100; depth = 'mono' }; 'sheet-6' = @{ w = 5100; h = 6600; depth = 'mono' }; 'sheet-7' = @{ w = 5100; h = 6600; depth = $colour }; maxMB = 60 }
  Run-Case "legal-actual-$tag" $q "[ $open, { `"tool`": `"doc_print`", `"args`": { `"doc`": 1, `"printer`": `"$q`", `"pages`": `"5`", `"paper`": `"Legal`", `"layout`": `"actual`" } } ]" @{
    'sheet-1' = @{ w = 5100; h = 8400; depth = 'mono'; bands = 9 }; maxMB = 8 }
  Run-Case "tabloid-actual-$tag" $q "[ $open, { `"tool`": `"doc_print`", `"args`": { `"doc`": 1, `"printer`": `"$q`", `"pages`": `"6`", `"paper`": `"Tabloid`", `"layout`": `"actual`" } } ]" @{
    'sheet-1' = @{ w = 6600; h = 10200; depth = 'mono'; bands = 10 }; maxMB = 12 }
  Run-Case "grayscale-$tag" $q "[ $open, { `"tool`": `"doc_print`", `"args`": { `"doc`": 1, `"printer`": `"$q`", `"pages`": `"2-3`", `"grayscale`": true } } ]" @{
    'sheet-1' = @{ w = 5100; h = 6600; depth = 'mono' }; 'sheet-2' = @{ w = 5100; h = 6600; depth = 'gray' }; maxMB = 12 }
  Run-Case "a4-shrink-$tag" $q "[ $open, { `"tool`": `"doc_print`", `"args`": { `"doc`": 1, `"printer`": `"$q`", `"pages`": `"1, 4`", `"paper`": `"A4`", `"layout`": `"shrink`" } } ]" @{
    'sheet-1' = @{ w = 4961; h = 7016; depth = 'mono' }; 'sheet-2' = @{ w = 7016; h = 4961; depth = 'mono' }; maxMB = 8 }
}
$mixed = '{ "tool": "doc_open", "args": { "path": "mixed.pdf" } }'
Run-Case "mixed-duplex-copies-ps3" 'DocuView Test PS3' "[ $mixed, { `"tool`": `"doc_print`", `"args`": { `"doc`": 1, `"printer`": `"DocuView Test PS3`", `"layout`": `"shrink`", `"duplex`": `"long-edge`", `"copies`": 2, `"collate`": true } } ]" @{
  'sheet-1' = @{ w = 5100; h = 6600 }; 'sheet-2' = @{ w = 6600; h = 5100 }; 'sheet-3' = @{ w = 5100; h = 6600 }; 'sheet-4' = @{ w = 5100; h = 6600 }; maxMB = 6 }
$results | Format-Table -AutoSize -Wrap
$results | ForEach-Object { "{0,-26} {1}" -f $_.case, $_.result }
"$(($results | Where-Object result -eq 'ok').Count) of $($results.Count) cases ok"
