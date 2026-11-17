# Measures this laptop's fan and cooling, to tune the fan-curve presets:
#   - RPM at each fan % (flat custom curve)
#   - temperatures at several fan speeds, at idle and under a fixed CPU load
# Writes a CSV (one row every ~3 s). Needs admin (PawnIO). Safe: the curve's
# two hottest points keep their factory floor (70 %, 85 %), a load step ends
# at 84 C, and the fan mode + curve from before are restored at the end.
# -ThenInstallWhen <file>: afterwards wait (max 30 min) for that file to
# appear, then run install.ps1 in this same admin session (one prompt for both).
param([Parameter(Mandatory)][string]$Out, [string]$ThenInstallWhen, [int]$LoadThreads = 4)
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
$cli = Join-Path $root 'target\release\dragonslayer-cli.exe'
$csv = Join-Path $Out 'fan-measure.csv'
$log = Join-Path $Out 'fan-measure.log'
function L($s) { "$(Get-Date -Format HH:mm:ss) $s" | Out-File $log -Append -Encoding utf8 }
"" | Out-File $log -Encoding utf8
'time_s,phase,set_pct,load,cpu_c,gpu_c,cpu_target,gpu_target,rpm,boost' | Out-File $csv -Encoding utf8

function Status {
  $t = & $cli --backend pawnio status 2>&1 | Out-String
  $o = @{ raw = $t }
  if ($t -match 'CPU\s+: (\d+) C, fan (\d+)%') { $o.cpu = [int]$Matches[1]; $o.cput = [int]$Matches[2] }
  if ($t -match 'GPU\s+: (\d+) C, fan (\d+)%') { $o.gpu = [int]$Matches[1]; $o.gput = [int]$Matches[2] }
  if ($t -match 'Fan speed\s+: (\d+) RPM') { $o.rpm = [int]$Matches[1] }
  if ($t -match 'Fan mode\s+: (\w+)') { $o.mode = $Matches[1] }
  if ($t -match 'CPU fan curve : \[([\d, ]+)\]') { $o.curve = ($Matches[1] -replace ' ', '') }
  $o.boost = [int]($t -match 'Cooler Boost\s+: ON')
  $o
}
function SetFlat([int]$p) {
  $c = @($p, $p, $p, $p, $p, [Math]::Max($p, 70), [Math]::Max($p, 85)) -join ','
  & $cli --backend pawnio curve $c 2>&1 | Out-Null
}
$sw = [Diagnostics.Stopwatch]::StartNew()
$script:load = @()
function Sample($phase, $p, $loadOn) {
  $s = Status
  "{0:N0},{1},{2},{3},{4},{5},{6},{7},{8},{9}" -f $sw.Elapsed.TotalSeconds, $phase, $p, [int]$loadOn, $s.cpu, $s.gpu, $s.cput, $s.gput, $s.rpm, $s.boost | Out-File $csv -Append -Encoding utf8
  $s
}
# holds one fan speed for $secs; returns $false if it got too hot (step cut short)
function Hold($phase, [int]$p, [int]$secs, $loadOn) {
  SetFlat $p
  $end = $sw.Elapsed.TotalSeconds + $secs
  while ($sw.Elapsed.TotalSeconds -lt $end) {
    Start-Sleep -Milliseconds 2600
    $s = Sample $phase $p $loadOn
    if ($loadOn -and $s.cpu -ge 84) { L "$phase ${p}%: reached $($s.cpu) C, step ended early"; return $false }
  }
  $true
}

$before = Status
L "before: mode=$($before.mode) curve=$($before.curve) cpu=$($before.cpu) gpu=$($before.gpu) rpm=$($before.rpm)"
try {
  # 1. idle: three speeds, a minute each (quietest last, so heat only builds)
  foreach ($p in 65, 50, 35) { Hold 'idle' $p 60 $false | Out-Null }
  # 2. RPM at every speed, 12 s each
  foreach ($p in 30, 40, 45, 55, 60, 70, 75, 80, 90, 100) { Hold 'rpm' $p 12 $false | Out-Null }
  # 3. fixed CPU load: fastest first; stop the sweep once a step gets too hot
  L "load on: $LoadThreads busy threads"
  $script:load = 1..$LoadThreads | ForEach-Object { Start-Process powershell -WindowStyle Hidden -PassThru -ArgumentList '-NoProfile', '-Command', 'while ($true) { }' }
  foreach ($p in 100, 85, 70, 55) { if (-not (Hold 'load' $p 60 $true)) { break } }
}
finally {
  $script:load | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
  L 'load off'
  # put back what was there
  if ($before.mode -eq 'Advanced' -and $before.curve) { & $cli --backend pawnio curve $before.curve 2>&1 | Out-Null }
  elseif ($before.mode -eq 'Silent') { & $cli --backend pawnio fan silent 2>&1 | Out-Null }
  else { & $cli --backend pawnio fan auto 2>&1 | Out-Null }
  $after = Status
  L "restored: mode=$($after.mode) curve=$($after.curve)"
  L 'measure done'
}

if ($ThenInstallWhen) {
  $cancel = "$ThenInstallWhen.cancel"
  for ($i = 0; $i -lt 900 -and -not (Test-Path -LiteralPath $ThenInstallWhen) -and -not (Test-Path -LiteralPath $cancel); $i++) { Start-Sleep -Seconds 2 }
  if (Test-Path -LiteralPath $ThenInstallWhen) {
    L 'installing'
    & (Join-Path $PSScriptRoot 'install.ps1') -Elevated -Log (Join-Path $Out 'install.log')
    L 'install done'
  } else { L 'no install requested' }
}
