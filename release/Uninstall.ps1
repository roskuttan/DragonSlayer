# DragonSlayer uninstaller. Gives the fan back to MSI's own curve, removes
# the app, its Start menu entry and its Task Scheduler tasks. Your settings
# and history in %APPDATA%\DragonSlayer are kept unless you say otherwise.
param([switch]$Elevated)
if (-not $Elevated) {
  Write-Host 'DragonSlayer uninstall: Windows will ask for admin rights once.'
  try {
    $p = Start-Process powershell -Verb RunAs -PassThru -ErrorAction Stop -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated')
    $p.WaitForExit()
  } catch { Write-Host 'Cancelled: nothing was removed.' }
  return
}

$pf = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion').ProgramFilesDir
$dest = Join-Path $pf 'DragonSlayer'
Get-Process dragonslayer -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
# hand the fan back to the laptop's own (MSI) curve
$cli = Join-Path $dest 'dragonslayer-cli.exe'
if (Test-Path $cli) { & $cli --backend pawnio fan auto | Out-Null }
foreach ($t in 'DragonSlayer', 'DragonSlayer Open') { schtasks /Delete /TN $t /F 2>$null | Out-Null }
Remove-Item -LiteralPath (Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'DragonSlayer.lnk') -ErrorAction SilentlyContinue
Set-Location $env:TEMP
Remove-Item -LiteralPath $dest -Recurse -Force -ErrorAction SilentlyContinue
Write-Host 'DragonSlayer is removed. The fan is back on the laptop''s own curve.' -ForegroundColor Green
$data = Join-Path $env:APPDATA 'DragonSlayer'
if (Test-Path $data) {
  $a = Read-Host "Also delete your settings and history in $data ? (y/N)"
  if ($a -match '^[yY]') { Remove-Item -LiteralPath $data -Recurse -Force; Write-Host 'Settings deleted.' }
}
Start-Sleep -Seconds 3
