# Launches the GUI (optionally with args), waits, and captures ONLY its own
# window contents with PrintWindow (works when the window is covered, never
# captures other windows, never steals focus).
# Usage: shot.ps1 -Out <out.png> [-Wait seconds] [-AppArgs args...]
param([string]$Out, [int]$Wait = 8, [string[]]$AppArgs = @())
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Shot {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[Shot]::SetProcessDPIAware() | Out-Null
Get-Process dragonslayer -ErrorAction SilentlyContinue | Stop-Process -Force
$exe = Join-Path $PSScriptRoot "..\target\release\dragonslayer.exe"
$p = if ($AppArgs.Count) { Start-Process $exe -ArgumentList $AppArgs -PassThru } else { Start-Process $exe -PassThru }
for ($i = 0; $i -lt 20 -and $p.MainWindowHandle -eq 0; $i++) { Start-Sleep -Milliseconds 500; $p.Refresh() }
Start-Sleep -Seconds $Wait
$h = $p.MainWindowHandle
$r = New-Object Shot+RECT; [Shot]::GetWindowRect($h, [ref]$r) | Out-Null
$bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$g = [Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [Shot]::PrintWindow($h, $hdc, 2)   # 2 = PW_RENDERFULLCONTENT (GPU windows)
$g.ReleaseHdc($hdc)
$bmp.Save($Out, [Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
"pid $($p.Id) $($r.R - $r.L)x$($r.B - $r.T) printwindow=$ok -> $Out"
