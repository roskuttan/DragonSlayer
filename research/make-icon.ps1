# Turns the logo image (red rounded tile on a plain black or white background) into:
#   assets/logo.png        1024x1024 master, transparent outside the tile
#   assets/icon-256.rgba   256x256 straight-alpha RGBA, embedded in the app
#                          (src/icon.rs makes every other size from it)
# Usage: make-icon.ps1 -Source <image.png|webp|jpg>
param([Parameter(Mandatory)][string]$Source)
$root = Split-Path $PSScriptRoot -Parent
$assets = Join-Path $root 'assets'
New-Item -ItemType Directory -Force $assets | Out-Null
Add-Type -AssemblyName PresentationCore, WindowsBase
Add-Type -ReferencedAssemblies PresentationCore, WindowsBase, System.Xaml @'
using System; using System.IO; using System.Windows.Media; using System.Windows.Media.Imaging;
public static class Cut {
  // background = the colour in the image's top-left corner (black or white)
  static byte[] bg = new byte[3];
  static bool White(byte[] p, int i) {
    return Math.Abs(p[i] - bg[0]) < 40 && Math.Abs(p[i + 1] - bg[1]) < 40 && Math.Abs(p[i + 2] - bg[2]) < 40;
  }
  // premultiplied float RGBA, area-averaged (exact box filter with partial pixels)
  public static float[] Resample(float[] s, int sw, int sh, int dw, int dh) {
    var d = new float[dw * dh * 4];
    double fx = (double)sw / dw, fy = (double)sh / dh;
    for (int y = 0; y < dh; y++) {
      double y0 = y * fy, y1 = (y + 1) * fy;
      for (int x = 0; x < dw; x++) {
        double x0 = x * fx, x1 = (x + 1) * fx;
        double r = 0, g = 0, b = 0, a = 0, wsum = 0;
        for (int sy = (int)y0; sy < Math.Min(sh, (int)Math.Ceiling(y1)); sy++) {
          double wy = Math.Min(y1, sy + 1) - Math.Max(y0, sy);
          for (int sx = (int)x0; sx < Math.Min(sw, (int)Math.Ceiling(x1)); sx++) {
            double w = wy * (Math.Min(x1, sx + 1) - Math.Max(x0, sx));
            int i = (sy * sw + sx) * 4;
            r += s[i] * w; g += s[i + 1] * w; b += s[i + 2] * w; a += s[i + 3] * w; wsum += w;
          }
        }
        int o = (y * dw + x) * 4;
        d[o] = (float)(r / wsum); d[o + 1] = (float)(g / wsum); d[o + 2] = (float)(b / wsum); d[o + 3] = (float)(a / wsum);
      }
    }
    return d;
  }
  // straight-alpha RGBA bytes from premultiplied floats
  public static byte[] Bytes(float[] f) {
    var o = new byte[f.Length];
    for (int i = 0; i < f.Length; i += 4) {
      float a = f[i + 3];
      for (int c = 0; c < 3; c++) o[i + c] = (byte)Math.Round(a > 0 ? Math.Min(255, f[i + c] / a * 255) : 0);
      o[i + 3] = (byte)Math.Round(a * 255);
    }
    return o;
  }
  public static string Run(string src, string pngOut, string rgbaOut) {
    var dec = BitmapDecoder.Create(new Uri(src), BitmapCreateOptions.None, BitmapCacheOption.OnLoad);
    var bmp = new FormatConvertedBitmap(dec.Frames[0], PixelFormats.Bgra32, null, 0);
    int w = bmp.PixelWidth, h = bmp.PixelHeight;
    var p = new byte[w * h * 4];
    bmp.CopyPixels(p, w * 4, 0);
    bg[0] = p[0]; bg[1] = p[1]; bg[2] = p[2];
    // tile bounds: first non-white row/column from each side, along the middle lines
    int L = 0, R = w - 1, T = 0, B = h - 1;
    while (White(p, ((h / 2) * w + L) * 4)) L++;
    while (White(p, ((h / 2) * w + R) * 4)) R--;
    while (White(p, (T * w + w / 2) * 4)) T++;
    while (White(p, (B * w + w / 2) * 4)) B--;
    // corner radius from the white gap along each corner's diagonal
    double gap = 0;
    int[,] corners = { { L, T, 1, 1 }, { R, T, -1, 1 }, { L, B, 1, -1 }, { R, B, -1, -1 } };
    for (int k = 0; k < 4; k++) {
      int t = 0;
      while (White(p, ((corners[k, 1] + corners[k, 3] * t) * w + corners[k, 0] + corners[k, 2] * t) * 4)) t++;
      gap += t / 4.0;
    }
    double rad = gap / (1 - 1 / Math.Sqrt(2));
    // mask inset 3 px so no white fringe survives; 4x4 supersampled coverage
    double inset = 3, lo_x = L + inset, hi_x = R + 1 - inset, lo_y = T + inset, hi_y = B + 1 - inset, rr = rad - inset;
    int side = Math.Max(R - L + 1, B - T + 1);
    int pad = (int)Math.Round(side * 0.03);
    int S = side + 2 * pad, ox = L - pad - (side - (R - L + 1)) / 2, oy = T - pad - (side - (B - T + 1)) / 2;
    var f = new float[S * S * 4];
    for (int y = 0; y < S; y++) for (int x = 0; x < S; x++) {
      int sx = x + ox, sy = y + oy;
      if (sx < 0 || sy < 0 || sx >= w || sy >= h) continue;
      int cov = 0;
      for (int a = 0; a < 4; a++) for (int b = 0; b < 4; b++) {
        double px = sx + (b + 0.5) / 4, py = sy + (a + 0.5) / 4;
        if (px < lo_x || px > hi_x || py < lo_y || py > hi_y) continue;
        double cx = Math.Min(Math.Max(px, lo_x + rr), hi_x - rr), cy = Math.Min(Math.Max(py, lo_y + rr), hi_y - rr);
        if ((px - cx) * (px - cx) + (py - cy) * (py - cy) <= rr * rr) cov++;
      }
      float al = cov / 16f; int i = (sy * w + sx) * 4, o = (y * S + x) * 4;
      f[o] = p[i + 2] / 255f * al; f[o + 1] = p[i + 1] / 255f * al; f[o + 2] = p[i] / 255f * al; f[o + 3] = al;
    }
    // 1024 master PNG
    var m = Bytes(Resample(f, S, S, 1024, 1024));
    var bgra = new byte[m.Length];
    for (int i = 0; i < m.Length; i += 4) { bgra[i] = m[i + 2]; bgra[i + 1] = m[i + 1]; bgra[i + 2] = m[i]; bgra[i + 3] = m[i + 3]; }
    var enc = new PngBitmapEncoder();
    enc.Frames.Add(BitmapFrame.Create(BitmapSource.Create(1024, 1024, 96, 96, PixelFormats.Bgra32, null, bgra, 1024 * 4)));
    using (var fs = File.Create(pngOut)) enc.Save(fs);
    // 256 RGBA for the app
    File.WriteAllBytes(rgbaOut, Bytes(Resample(f, S, S, 256, 256)));
    return string.Format("source {0}x{1}, tile {2}..{3} x {4}..{5}, corner radius {6:F0} px", w, h, L, R, T, B, rad);
  }
}
'@
[Cut]::Run((Resolve-Path $Source).Path, (Join-Path $assets 'logo.png'), (Join-Path $assets 'icon-256.rgba'))
