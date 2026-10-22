# SPDX-FileCopyrightText: 2026 Tauminus contributors
# SPDX-License-Identifier: GPL-3.0-or-later

# Runs the in-app benchmark on Windows and samples memory once a second;
# the Windows half of ci/bench.sh.
#
#   pwsh ci/bench.ps1 <app.exe> <dir with xtask.exe> <output dir>
#
# WebView2 runs as msedgewebview2.exe processes under the app; the app's
# shells (cmd.exe, conhost.exe, xtask.exe) are left out of the webview sum.

param(
  [Parameter(Mandatory)] [string] $App,
  [Parameter(Mandatory)] [string] $Tools,
  [Parameter(Mandatory)] [string] $Out
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$report = Join-Path $Out 'report.json'
$samples = Join-Path $Out 'samples.csv'
Remove-Item -ErrorAction Ignore $report, $samples

function Now-Ms { [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds() }

# The process with this id and all its descendants, from one process listing.
function Get-Tree([int] $Root) {
  $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, WorkingSetSize
  $children = @{}
  foreach ($p in $all) {
    if (-not $children.ContainsKey([int]$p.ParentProcessId)) { $children[[int]$p.ParentProcessId] = @() }
    $children[[int]$p.ParentProcessId] += $p
  }
  $tree = @($all | Where-Object { $_.ProcessId -eq $Root })
  $queue = [System.Collections.Generic.Queue[int]]::new()
  $queue.Enqueue($Root)
  while ($queue.Count -gt 0) {
    $id = $queue.Dequeue()
    foreach ($child in $children[$id]) {
      if ($child.ProcessId -ne $id) {
        $tree += $child
        $queue.Enqueue([int]$child.ProcessId)
      }
    }
  }
  $tree
}

$env:PATH = "$Tools;$env:PATH"
$env:TAUMINUS_BENCH_OUT = $report
Set-Content -Path (Join-Path $Out 'launched_ms') -Value (Now-Ms)
$process = Start-Process -FilePath $App -PassThru -RedirectStandardError (Join-Path $Out 'app.log')

$deadline = (Get-Date).AddMinutes(20)
while (-not ((Test-Path $report) -and (Get-Item $report).Length -gt 0) -and (Get-Date) -lt $deadline -and -not $process.HasExited) {
  $tree = Get-Tree $process.Id
  $appKiB = [math]::Round((($tree | Where-Object { $_.ProcessId -eq $process.Id }).WorkingSetSize | Measure-Object -Sum).Sum / 1024)
  $webKiB = [math]::Round((($tree | Where-Object { $_.Name -eq 'msedgewebview2.exe' }).WorkingSetSize | Measure-Object -Sum).Sum / 1024)
  Add-Content -Path $samples -Value "$(Now-Ms),$appKiB,$webKiB"
  Start-Sleep -Seconds 1
}

foreach ($p in (Get-Tree $process.Id)) { Stop-Process -Id $p.ProcessId -Force -ErrorAction Ignore }
if (-not ((Test-Path $report) -and (Get-Item $report).Length -gt 0)) {
  Get-Content -ErrorAction Ignore (Join-Path $Out 'app.log') | Select-Object -Last 50
  throw 'the benchmark wrote no report'
}
python (Join-Path $PSScriptRoot 'bench_summary.py') $Out
