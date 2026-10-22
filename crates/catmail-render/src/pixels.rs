//! From what the rasterizers produce to what the UI shows.

use crate::Tile;
use crate::worker::Pixmap;

/// Premultiplied RGBA drawn over `canvas` (RGB), as opaque BGRA (GPUI's image layout).
pub(crate) fn tile(pixmap: Pixmap, canvas: [u8; 3]) -> Tile {
    let Pixmap {
        width,
        height,
        mut rgba,
    } = pixmap;
    let [cr, cg, cb] = canvas.map(u16::from);
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let [r, g, b, a] = *pixel;
        *pixel = match a {
            // Most of a page: opaque, or nothing painted.
            255 => [b, g, r, 255],
            0 => [canvas[2], canvas[1], canvas[0], 255],
            // Premultiplied: c + canvas × (1 - α), which never exceeds 255 since c ≤ α.
            _ => {
                let behind = u16::from(255 - a);
                let over = |c: u8, canvas: u16| c + ((canvas * behind + 127) / 255) as u8;
                [over(b, cb), over(g, cg), over(r, cr), 255]
            }
        };
    }
    Tile {
        width,
        height,
        bgra: rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composites_over_white_and_swaps_to_bgra() {
        let pixmap = Pixmap {
            width: 3,
            height: 1,
            // Transparent, opaque red, half-transparent blue (premultiplied).
            rgba: vec![0, 0, 0, 0, 255, 0, 0, 255, 0, 0, 128, 128],
        };
        let tile = tile(pixmap, [255; 3]);
        assert_eq!(
            tile.bgra,
            [255, 255, 255, 255, 0, 0, 255, 255, 255, 127, 127, 255]
        );
    }

    #[test]
    fn composites_over_the_canvas() {
        let pixmap = Pixmap {
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 0, 0, 0, 128, 128],
        };
        let tile = tile(pixmap, [16, 32, 64]);
        assert_eq!(tile.bgra, [64, 32, 16, 255, 160, 16, 8, 255]);
    }
}
