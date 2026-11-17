# Records the demo app's Dashboard card (gauges + spinning fan) and saves a
# looping animated GIF: docs/demo-live.gif. Uses only Windows' own image
# libraries. Run research/shot-demo.ps1 first (it builds the demo exe).
# Usage: make-gif.ps1 [-Seconds 3] [-Fps 12] [-Width 640]
param([double]$Seconds = 3.0, [int]$Fps = 12, [int]$Width = 640)
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root 'target\demo\release\dragonslayer-demo.exe'
$out = Join-Path $root 'docs\demo-live.gif'
Add-Type -AssemblyName System.Drawing, PresentationCore, WindowsBase
Add-Type -ReferencedAssemblies System.Drawing, PresentationCore, WindowsBase, System.Xaml @'
using System; using System.IO; using System.Collections.Generic; using System.Runtime.InteropServices;
using System.Windows.Media; using System.Windows.Media.Imaging;
public static class Gif {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();

  // one frame of the window, cropped to (x0,y0)-(x1,y1) and scaled to width w
  public static System.Drawing.Bitmap Grab(IntPtr h, int x0, int y0, int x1, int y1, int w) {
    RECT r; GetWindowRect(h, out r);
    using (var full = new System.Drawing.Bitmap(r.R - r.L, r.B - r.T)) {
      using (var g = System.Drawing.Graphics.FromImage(full)) { var hdc = g.GetHdc(); PrintWindow(h, hdc, 2); g.ReleaseHdc(hdc); }
      int cw = x1 - x0, ch = y1 - y0, hh = (int)Math.Round((double)ch * w / cw);
      var outb = new System.Drawing.Bitmap(w, hh);
      using (var g = System.Drawing.Graphics.FromImage(outb)) {
        g.InterpolationMode = System.Drawing.Drawing2D.InterpolationMode.HighQualityBicubic;
        g.DrawImage(full, new System.Drawing.Rectangle(0, 0, w, hh), new System.Drawing.Rectangle(x0, y0, cw, ch), System.Drawing.GraphicsUnit.Pixel);
      }
      return outb;
    }
  }

  public static void Save(List<System.Drawing.Bitmap> frames, int delayCs, string path) {
    var enc = new GifBitmapEncoder();
    foreach (var f in frames) {
      BitmapSource src;
      using (var ms = new MemoryStream()) {
        f.Save(ms, System.Drawing.Imaging.ImageFormat.Png); ms.Position = 0;
        src = BitmapFrame.Create(ms, BitmapCreateOptions.None, BitmapCacheOption.OnLoad);
      }
      // a palette made from this frame's own colours (not a fixed web palette)
      var idx = new FormatConvertedBitmap(src, PixelFormats.Indexed8, new BitmapPalette(src, 256), 0);
      var meta = new BitmapMetadata("gif");
      meta.SetQuery("/grctlext/Delay", (ushort)delayCs);
      meta.SetQuery("/grctlext/Disposal", (byte)1);
      enc.Frames.Add(BitmapFrame.Create(idx, null, meta, null));
    }
    byte[] gif;
    using (var ms = new MemoryStream()) { enc.Save(ms); gif = ms.ToArray(); }
    // WPF writes neither frame timing nor looping. Rebuild the file: header,
    // screen descriptor, global colour table, then a NETSCAPE2.0 "loop
    // forever" block, and a timing block (graphic control extension) in
    // front of every frame; WPF's own extension blocks are dropped.
    var o = new MemoryStream();
    int pos = 13;
    if ((gif[10] & 0x80) != 0) pos += 3 * (1 << ((gif[10] & 7) + 1));
    o.Write(gif, 0, pos);
    byte[] loop = { 0x21, 0xFF, 0x0B, (byte)'N', (byte)'E', (byte)'T', (byte)'S', (byte)'C', (byte)'A', (byte)'P', (byte)'E', (byte)'2', (byte)'.', (byte)'0', 0x03, 0x01, 0x00, 0x00, 0x00 };
    o.Write(loop, 0, loop.Length);
    while (pos < gif.Length) {
      byte b = gif[pos];
      if (b == 0x3B) { o.WriteByte(0x3B); break; }   // trailer
      if (b == 0x21) {                                 // extension: skip it
        pos += 2;
        while (gif[pos] != 0) pos += gif[pos] + 1;
        pos += 1;
        continue;
      }
      if (b != 0x2C) throw new InvalidDataException("unexpected GIF block 0x" + b.ToString("X2"));
      byte[] gce = { 0x21, 0xF9, 0x04, 0x04, (byte)(delayCs & 0xFF), (byte)(delayCs >> 8), 0x00, 0x00 };
      o.Write(gce, 0, gce.Length);
      int start = pos;
      byte packed = gif[pos + 9];
      pos += 10;                                       // image descriptor
      if ((packed & 0x80) != 0) pos += 3 * (1 << ((packed & 7) + 1)); // local colour table
      pos += 1;                                        // LZW minimum code size
      while (gif[pos] != 0) pos += gif[pos] + 1;       // image data sub-blocks
      pos += 1;
      o.Write(gif, start, pos - start);
    }
    File.WriteAllBytes(path, o.ToArray());
  }
}
'@
[Gif]::SetProcessDPIAware() | Out-Null
$p = Start-Process $exe -ArgumentList '--mock' -PassThru
for ($i = 0; $i -lt 20 -and $p.MainWindowHandle -eq 0; $i++) { Start-Sleep -Milliseconds 500; $p.Refresh() }
Start-Sleep -Seconds 4
$h = $p.MainWindowHandle
# the Dashboard's top-left card at 125 % scaling: x 18..648, y 74..416 (pixels)
$frames = New-Object 'System.Collections.Generic.List[System.Drawing.Bitmap]'
$n = [int]($Seconds * $Fps); $sw = [Diagnostics.Stopwatch]::StartNew()
for ($k = 0; $k -lt $n; $k++) {
  while ($sw.Elapsed.TotalSeconds -lt $k / $Fps) { Start-Sleep -Milliseconds 2 }
  $frames.Add([Gif]::Grab($h, 18, 74, 648, 416, $Width))
}
Stop-Process -Id $p.Id -Force
# play back at the speed it was really captured (grabbing can be slower than asked)
[Gif]::Save($frames, [int][Math]::Round($sw.Elapsed.TotalSeconds * 100 / $n), $out)
"{0} frames in {1:N1} s -> {2} ({3:N0} KB)" -f $n, $sw.Elapsed.TotalSeconds, $out, ((Get-Item $out).Length / 1KB)
