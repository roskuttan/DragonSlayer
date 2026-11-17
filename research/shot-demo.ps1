# Screenshots of the demo (--mock) app without an admin prompt.
# Builds a fast (no-LTO) copy of the GUI into a separate folder,
# opens it once per tab and captures ONLY its own window with PrintWindow.
# Never touches a running DragonSlayer.
# Usage: shot-demo.ps1 [-Tabs dashboard,fans,...] [-OutDir docs]
param([string[]]$Tabs = @('dashboard', 'fans', 'tools', 'system', 'automation'), [string]$OutDir)
$root = Split-Path $PSScriptRoot -Parent
if (-not $OutDir) { $OutDir = Join-Path $root 'docs' }
$target = Join-Path $root 'target\demo'
$env:CARGO_TARGET_DIR = $target
$env:CARGO_PROFILE_RELEASE_LTO = 'false'
$env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '16'
Push-Location $root
cargo build --release --bin dragonslayer 2>&1 | Select-Object -Last 1
Pop-Location
$exe = Join-Path $target 'release\dragonslayer-demo.exe'
Copy-Item (Join-Path $target 'release\dragonslayer.exe') $exe -Force

Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class DemoShot {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
'@
[DemoShot]::SetProcessDPIAware() | Out-Null
foreach ($t in $Tabs) {
  $p = Start-Process $exe -ArgumentList @('--mock', '--tab', $t) -PassThru
  for ($i = 0; $i -lt 20 -and $p.MainWindowHandle -eq 0; $i++) { Start-Sleep -Milliseconds 500; $p.Refresh() }
  Start-Sleep -Seconds 5
  $h = $p.MainWindowHandle
  $r = New-Object DemoShot+RECT; [DemoShot]::GetWindowRect($h, [ref]$r) | Out-Null
  $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  $g = [Drawing.Graphics]::FromImage($bmp)
  $hdc = $g.GetHdc()
  [DemoShot]::PrintWindow($h, $hdc, 2) | Out-Null
  $g.ReleaseHdc($hdc)
  $out = Join-Path $OutDir "demo-$t.png"
  $bmp.Save($out, [Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
  Stop-Process -Id $p.Id -Force
  "$t -> $out"
}
