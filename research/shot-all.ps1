# Run elevated: captures each tab of the (admin) app into docs\screenshot-<tab>.png
$here = $PSScriptRoot
$docs = Join-Path (Split-Path $here -Parent) 'docs'
$log = Join-Path $here 'shot-log.txt'
"" | Set-Content $log
foreach ($t in 'dashboard', 'system', 'automation') {
  & (Join-Path $here 'shot.ps1') -Out (Join-Path $docs "screenshot-$t.png") -Wait 14 -AppArgs @('--tab', $t) *>> $log
}
Get-Process dragonslayer -ErrorAction SilentlyContinue | Stop-Process -Force
