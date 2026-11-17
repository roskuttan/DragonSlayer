# DragonSlayer installer (for the downloaded release zip).
# Copies the app to C:\Program Files\DragonSlayer and starts it once, which
# sets up the no-prompt launch. Asks for admin once. Creates no shortcuts:
# open DragonSlayer from the Start menu search, or pin it from the taskbar.
param([switch]$Elevated)
$here = $PSScriptRoot
if (-not $Elevated) {
  Write-Host 'DragonSlayer setup: Windows will ask for admin rights once.'
  try {
    $p = Start-Process powershell -Verb RunAs -PassThru -ErrorAction Stop -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated')
    $p.WaitForExit()
  } catch { Write-Host 'Cancelled: nothing was installed.' }
  return
}

function Fail($msg) { Write-Host ''; Write-Host $msg -ForegroundColor Yellow; Read-Host 'Press Enter to close'; exit 1 }

$pf = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion').ProgramFilesDir
if (-not (Test-Path (Join-Path $pf 'PawnIO\PawnIOLib.dll'))) {
  Fail "PawnIO is not installed. DragonSlayer needs it to talk to the laptop's controller.`nInstall it from https://github.com/namazso/PawnIO/releases (or https://pawnio.eu), then run this again."
}
foreach ($f in 'dragonslayer.exe', 'dragonslayer-cli.exe', 'LpcACPIEC.bin') {
  if (-not (Test-Path (Join-Path $here $f))) { Fail "$f is missing next to this script. Unzip the whole download first, then run Install again." }
}

$dest = Join-Path $pf 'DragonSlayer'
Write-Host "Installing to $dest"
Get-Process dragonslayer -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
New-Item -ItemType Directory -Force $dest | Out-Null
foreach ($f in 'dragonslayer.exe', 'dragonslayer-cli.exe', 'LpcACPIEC.bin', 'COPYING-PawnIO.txt', 'LICENSE.txt', 'README.txt', 'Uninstall.ps1', 'Uninstall.cmd') {
  $src = Join-Path $here $f
  if (Test-Path $src) { Copy-Item -LiteralPath $src -Destination $dest -Force }
}

# Start Menu entry, so it can be found by searching "DragonSlayer"
$menu = Join-Path ([Environment]::GetFolderPath('CommonPrograms')) 'DragonSlayer.lnk'
$sh = New-Object -ComObject WScript.Shell
$l = $sh.CreateShortcut($menu); $l.TargetPath = Join-Path $dest 'dragonslayer.exe'; $l.WorkingDirectory = $dest; $l.Description = 'DragonSlayer'; $l.Save()

# first start as admin: registers the no-prompt "DragonSlayer Open" task
Start-Process (Join-Path $dest 'dragonslayer.exe')
Write-Host ''
Write-Host 'Done. DragonSlayer is running (look for it in the tray).' -ForegroundColor Green
Write-Host 'Open it any time from the Start menu: search "DragonSlayer". It will not ask for admin again.'
Write-Host 'Turn on "Start with Windows" in its Automation tab if you want it at every login.'
Start-Sleep -Seconds 6
