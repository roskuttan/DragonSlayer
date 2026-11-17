# Installs the release build to C:\Program Files\DragonSlayer (admin-only
# folder, so the no-prompt "DragonSlayer Open" task is safe), points the
# desktop shortcut and taskbar pin at it, and tests a plain click:
#   1. app not running -> starts through the task, no UAC prompt
#   2. app running     -> a second click only brings it forward (one copy)
# Run from a normal PowerShell: asks for admin once. Rebuild first:
#   cargo build --release
param([switch]$Elevated, [string]$Log)
$root = Split-Path $PSScriptRoot -Parent
if (-not $Elevated) {
  $Log = Join-Path $env:TEMP 'dragonslayer-install.log'
  Remove-Item -LiteralPath $Log -ErrorAction SilentlyContinue   # never show a previous run's log
  try {
    $p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -PassThru -ErrorAction Stop -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Elevated', '-Log', "`"$Log`"")
  } catch {
    'NOT INSTALLED: the admin prompt was cancelled. Nothing was changed.'
    return
  }
  $p.WaitForExit()
  if (Test-Path -LiteralPath $Log) { Get-Content $Log } else { 'NOT INSTALLED: the admin part wrote no log.' }
  return
}
function L($s) { $s | Out-File $Log -Append -Encoding utf8 }
"install started $(Get-Date)" | Out-File $Log -Encoding utf8

$pf = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion').ProgramFilesDir
$dest = Join-Path $pf 'DragonSlayer'
$exe = Join-Path $dest 'dragonslayer.exe'
New-Item -ItemType Directory -Force $dest | Out-Null
Get-Process dragonslayer -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1
foreach ($f in @('target\release\dragonslayer.exe', 'target\release\dragonslayer-cli.exe', 'vendor\pawnio\LpcACPIEC.bin', 'vendor\pawnio\COPYING')) {
  Copy-Item -LiteralPath (Join-Path $root $f) -Destination $dest -Force
  L "copied $f"
}

# shortcuts: desktop + the taskbar pin from the MSI Control days
$sh = New-Object -ComObject WScript.Shell
$desk = [Environment]::GetFolderPath('Desktop')
$links = @((Join-Path $desk 'DragonSlayer.lnk'), "$env:APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\MSI Control.lnk", "$env:APPDATA\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\DragonSlayer.lnk")
foreach ($lnk in $links) {
  if (-not (Test-Path -LiteralPath $lnk) -and $lnk -notlike '*Desktop*') { continue }
  $l = $sh.CreateShortcut($lnk); $l.TargetPath = $exe; $l.WorkingDirectory = $dest; $l.IconLocation = "$exe,0"; $l.Description = 'DragonSlayer'; $l.Save()
  L "shortcut -> installed exe: $lnk"
}

# first start as admin: registers the "DragonSlayer Open" task, moves the logon task here
Start-Process $exe | Out-Null
for ($i = 0; $i -lt 40 -and -not (schtasks /Query /TN 'DragonSlayer Open' 2>$null); $i++) { Start-Sleep -Milliseconds 250 }
L "open task registered: $([bool](schtasks /Query /TN 'DragonSlayer Open' 2>$null))"
Start-Sleep -Seconds 2
Get-Process dragonslayer -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

function LastRun { (schtasks /Query /TN 'DragonSlayer Open' /V /FO CSV 2>$null | ConvertFrom-Csv).'Last Run Time' }
# explorer starts it like a click: as the normal user, without admin
$before = LastRun
Start-Process explorer.exe -ArgumentList "`"$exe`""
Start-Sleep -Seconds 8
$n = @(Get-Process dragonslayer -ErrorAction SilentlyContinue).Count
L "test 1 (not running, plain click): copies running = $n, task ran = $((LastRun) -ne $before)"
Start-Process explorer.exe -ArgumentList "`"$exe`""
Start-Sleep -Seconds 5
$n = @(Get-Process dragonslayer -ErrorAction SilentlyContinue).Count
L "test 2 (already running, click again): copies running = $n"
L "logon task runs: $(([xml](schtasks /Query /TN 'DragonSlayer' /XML 2>$null | Out-String)).Task.Actions.Exec.Command)"
L "open task runs:  $(([xml](schtasks /Query /TN 'DragonSlayer Open' /XML 2>$null | Out-String)).Task.Actions.Exec.Command)"
L "done $(Get-Date)"
