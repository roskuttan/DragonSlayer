# Elevated PawnIO test. Writes results to pawnio-test.txt next to this script.
# Only write: keyboard light -> 2 (Mid) -> 0 (Off).
$ErrorActionPreference = 'Continue'
$root = Split-Path $PSScriptRoot -Parent
$cli = Join-Path $root 'target\release\dragonslayer-cli.exe'
$out = Join-Path $PSScriptRoot 'pawnio-test.txt'
$log = New-Object System.Collections.Generic.List[string]
function L($s) { $log.Add([string]$s) }
function Run($a) { $r = & $cli @a 2>&1 | ForEach-Object { "$_" }; L "> dragonslayer-cli $($a -join ' ')  (exit $LASTEXITCODE)"; $r | ForEach-Object { L "  $_" }; return $r }

L "elevated: $(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"
Run @('--backend','pawnio','status') | Out-Null

# Compare static bytes (firmware string 0xA0-0xBF) and settings between drivers
$p = Run @('--backend','pawnio','dump')
$w = Run @('--backend','winio','dump')
foreach ($row in 'A0:','B0:','F0:','90:','E0:') {
  $pr = ($p | Where-Object { $_ -like "$row*" }); $wr = ($w | Where-Object { $_ -like "$row*" })
  L "compare $row  same=$($pr -eq $wr)"
}

# Timing: 20 status reads
$sw = [Diagnostics.Stopwatch]::StartNew()
1..20 | ForEach-Object { & $cli --backend pawnio status *> $null }
L "20 status runs: $($sw.ElapsedMilliseconds) ms"

# One harmless write and restore
Run @('--backend','pawnio','kbd','2') | Out-Null
Start-Sleep -Seconds 3
Run @('--backend','winio','status') | Where-Object { $_ -match 'Keyboard' } | ForEach-Object { L "cross-check via MSI driver: $_" }
Run @('--backend','pawnio','kbd','0') | Out-Null
$log | Set-Content -Path $out -Encoding UTF8
