# Controlled CPU stress test, to see how the running fan mode copes with
# sustained and bursty load. Needs no admin: the app already logs every 5 s
# to %APPDATA%\DragonSlayer\history\<day>.csv; this script only makes load
# and writes phase markers so the log can be read afterwards.
# Safety: a phase ends early if the log shows the CPU at 96 C or more twice
# in a row (heat protection may be off). All load stops on exit or error.
# Usage: stress-smart.ps1 -Out <folder>
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Continue'
$cores = [Environment]::ProcessorCount
$hist = "$env:APPDATA\DragonSlayer\history\$(Get-Date -Format 'yyyy-MM-dd').csv"
$marks = Join-Path $Out 'stress-phases.csv'
'phase,threads,start,end,ended_early' | Out-File $marks -Encoding utf8
$script:busy = @()

function Load([int]$n) {
  Unload
  $script:busy = @(1..$n | ForEach-Object { Start-Process powershell -WindowStyle Hidden -PassThru -ArgumentList '-NoProfile', '-Command', 'while ($true) { }' })
}
function Unload {
  $script:busy | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
  $script:busy = @()
}
function LastCpu {
  $l = Get-Content $hist -Tail 1 -ErrorAction SilentlyContinue
  if ($l -and $l -notmatch '^time') { [int]($l.Split(',')[2]) } else { 0 }
}
# runs for $secs with $threads busy (0 = idle); returns $true if cut short by heat
function Phase([string]$name, [int]$threads, [int]$secs) {
  $start = Get-Date -Format 'HH:mm:ss'
  if ($threads -gt 0) { Load $threads } else { Unload }
  $end = (Get-Date).AddSeconds($secs); $hot = 0; $early = $false
  while ((Get-Date) -lt $end) {
    Start-Sleep -Seconds 5
    if ((LastCpu) -ge 96) { $hot++ } else { $hot = 0 }
    if ($hot -ge 2 -and $threads -gt 0) { $early = $true; break }
  }
  Unload
  "$name,$threads,$start,$(Get-Date -Format 'HH:mm:ss'),$early" | Out-File $marks -Append -Encoding utf8
  $early
}

try {
  if (-not (Test-Path $hist)) { throw "no history log at $hist: is DragonSlayer running?" }
  Phase 'idle' 0 120 | Out-Null
  Phase 'medium' 3 240 | Out-Null
  Phase 'heavy' $cores 300 | Out-Null
  $burstStart = Get-Date -Format 'HH:mm:ss'
  for ($i = 1; $i -le 8; $i++) {
    if (Phase "burst$i" $cores 10) { break }
    Phase "gap$i" 0 20 | Out-Null
  }
  Phase 'cooldown' 0 180 | Out-Null
}
finally {
  Unload
  'done' | Out-File (Join-Path $Out 'stress-done.txt') -Encoding utf8
}
