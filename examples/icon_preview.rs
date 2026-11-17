//! Writes a preview strip of the app icon at the sizes Windows uses
//! (16..64 px, each shown actual-size and 4x enlarged) to the path given.
//! Run: cargo run --example icon_preview -- preview.bmp

fn main() {
    let out = std::env::args().nth(1).expect("output .bmp path");
    let sizes = [16u32, 20, 24, 32, 48, 64];
    let zoom = 4u32;
    let (gap, h) = (16u32, 64 * zoom + 2 * 16);
    let w: u32 = sizes.iter().map(|s| s + gap + s * zoom + gap).sum::<u32>() + gap;
    let bg = [24u8, 26, 32]; // like the dark taskbar
    let mut px = vec![0u8; (w * h * 4) as usize];
    for p in px.chunks_mut(4) {
        p.copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
    }
    let mut x0 = gap;
    for &s in &sizes {
        let icon = dragonslayer::icon::rgba(s);
        for (scale, ox) in [(1, x0), (zoom, x0 + s + gap)] {
            for y in 0..s * scale {
                for x in 0..s * scale {
                    let i = (((y / scale) * s + x / scale) * 4) as usize;
                    let a = f32::from(icon[i + 3]) / 255.0;
                    let o = (((gap + y) * w + ox + x) * 4) as usize;
                    for c in 0..3 {
                        px[o + c] = (f32::from(icon[i + c]) * a + f32::from(bg[c]) * (1.0 - a)).round() as u8;
                    }
                }
            }
        }
        x0 += s + gap + s * zoom + gap;
    }
    let mut bmp = Vec::new();
    bmp.extend_from_slice(b"BM");
    for v in [54 + w * h * 4, 0, 54, 40] {
        bmp.extend_from_slice(&v.to_le_bytes());
    }
    bmp.extend_from_slice(&(w as i32).to_le_bytes());
    bmp.extend_from_slice(&(-(h as i32)).to_le_bytes());
    bmp.extend_from_slice(&[1, 0, 32, 0]);
    bmp.extend_from_slice(&[0u8; 24]);
    for p in px.chunks(4) {
        bmp.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    std::fs::write(out, bmp).expect("write preview");
}
