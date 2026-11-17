//! The app icon: the DragonSlayer logo (sword through a dragon wing on a red
//! tile). `assets/icon-256.rgba` is made from `assets/logo.png` by
//! `research/make-icon.ps1`; every other size is area-averaged from it here.
//! Used for the window, the tray, the title bar and (via build.rs) the .exe
//! icon. No dependencies, so build.rs can include this file.

const MASTER: usize = 256;
static MASTER_RGBA: &[u8] = include_bytes!("../assets/icon-256.rgba");

/// RGBA pixels (straight alpha), `size` x `size` (at most 256).
pub fn rgba(size: u32) -> Vec<u8> {
    let d = size as usize;
    assert!(d > 0 && d <= MASTER, "icon sizes 1..=256");
    if d == MASTER {
        return MASTER_RGBA.to_vec();
    }
    // exact box filter over premultiplied colour, partial pixels weighted
    let f = MASTER as f64 / d as f64;
    let mut out = vec![0u8; d * d * 4];
    for y in 0..d {
        let (y0, y1) = (y as f64 * f, (y + 1) as f64 * f);
        for x in 0..d {
            let (x0, x1) = (x as f64 * f, (x + 1) as f64 * f);
            let mut acc = [0f64; 4];
            let mut wsum = 0.0;
            for sy in y0 as usize..(y1.ceil() as usize).min(MASTER) {
                let wy = y1.min(sy as f64 + 1.0) - y0.max(sy as f64);
                for sx in x0 as usize..(x1.ceil() as usize).min(MASTER) {
                    let w = wy * (x1.min(sx as f64 + 1.0) - x0.max(sx as f64));
                    let i = (sy * MASTER + sx) * 4;
                    let a = f64::from(MASTER_RGBA[i + 3]) / 255.0;
                    for c in 0..3 {
                        acc[c] += f64::from(MASTER_RGBA[i + c]) * a * w;
                    }
                    acc[3] += a * w;
                    wsum += w;
                }
            }
            let o = (y * d + x) * 4;
            let a = acc[3] / wsum;
            for c in 0..3 {
                out[o + c] = if a > 0.0 { (acc[c] / wsum / a).round().min(255.0) as u8 } else { 0 };
            }
            out[o + 3] = (a * 255.0).round() as u8;
        }
    }
    out
}

/// A Windows .ico file containing the icon at the given sizes (32-bit BMP entries).
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &sz in sizes {
        let px = rgba(sz);
        let mask_row = sz.div_ceil(32) * 4;
        let mut img = Vec::new();
        // BITMAPINFOHEADER (height doubled: colour + AND mask)
        for v in [40u32, sz, sz * 2] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        img.extend_from_slice(&1u16.to_le_bytes());
        img.extend_from_slice(&32u16.to_le_bytes());
        for v in [0u32, sz * sz * 4 + mask_row * sz, 0, 0, 0, 0] {
            img.extend_from_slice(&v.to_le_bytes());
        }
        // pixels, bottom-up, BGRA
        for y in (0..sz).rev() {
            for x in 0..sz {
                let i = ((y * sz + x) * 4) as usize;
                img.extend_from_slice(&[px[i + 2], px[i + 1], px[i], px[i + 3]]);
            }
        }
        img.extend(std::iter::repeat_n(0u8, (mask_row * sz) as usize));
        images.push(img);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&sz, img) in sizes.iter().zip(&images) {
        let d = if sz >= 256 { 0 } else { sz as u8 };
        out.extend_from_slice(&[d, d, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend(img);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_at_every_size() {
        assert_eq!(MASTER_RGBA.len(), MASTER * MASTER * 4);
        for s in [16u32, 20, 24, 32, 48, 64, 256] {
            let p = rgba(s);
            assert_eq!(p.len(), (s * s * 4) as usize);
            let px = |x: u32, y: u32| &p[((y * s + x) * 4) as usize..((y * s + x) * 4 + 4) as usize];
            assert_eq!(px(0, 0)[3], 0, "{s}px: corner transparent");
            let top = px(s / 2, s / 8);
            assert!(top[3] == 255 && top[0] > 150 && top[1] < 80, "{s}px: red tile, got {top:?}");
            let white = p.chunks(4).filter(|c| c[3] == 255 && c[0] > 200 && c[1] > 200 && c[2] > 200).count();
            assert!(white * 100 > (s * s) as usize * 5, "{s}px: the white sword is there");
        }
    }

    #[test]
    fn ico_layout() {
        let f = ico(&[16, 32, 256]);
        assert_eq!(&f[..6], &[0, 0, 1, 0, 3, 0]);
        assert_eq!(f[6 + 32], 0, "256 px stored as 0");
    }
}
