//! The overlay's picture: two lines of text in a 5x7 pixel font on a dark box, as one byte per
//! pixel. The Vulkan layer copies it into the swapchain image and the OpenGL hook clears it in
//! runs, so neither needs shaders, a font atlas or blending.

use formalpacer_shm::VrrState;

pub const BACKGROUND: u8 = 0;
pub const TEXT: u8 = 1;
pub const DIM: u8 = 2;

const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;
/// Cells between glyphs, between lines and around the text.
const SPACING: usize = 1;
const LEADING: usize = 3;
const PADDING: usize = 3;

/// Gap from the top-left corner of the swapchain, in cells.
pub const MARGIN: u32 = 4;

/// RGBA for each pixel value.
pub const COLORS: [[u8; 4]; 3] = [[16, 16, 18, 255], [242, 242, 242, 255], [150, 150, 156, 255]];

/// What the overlay says.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub fps: u32,
    /// The cap in fps, 0 for none.
    pub cap: u32,
    pub vrr: VrrState,
}

impl Text {
    pub fn lines(&self) -> [String; 2] {
        let vrr = match self.vrr {
            VrrState::On => "VRR ON",
            VrrState::Off => "VRR OFF",
            VrrState::Unsupported => "NO VRR",
            VrrState::Unknown => "",
        };
        let cap = if self.cap > 0 { format!("CAP {}", self.cap) } else { "UNCAPPED".to_owned() };
        let second = if vrr.is_empty() { cap } else { format!("{cap}  {vrr}") };
        [format!("{} FPS", self.fps), second]
    }
}

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Size of one font cell in pixels.
    pub scale: u32,
    /// Row-major, one of [`BACKGROUND`], [`TEXT`], [`DIM`] per pixel.
    pub pixels: Vec<u8>,
}

/// Font cells scale with the swapchain so the text reads the same on 1080p and 4K.
pub fn scale_for(swap_height: u32) -> u32 {
    (swap_height / 480).clamp(1, 8)
}

impl Bitmap {
    pub fn render(text: &Text, scale: u32) -> Self {
        let lines = text.lines();
        let chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let cols = PADDING * 2 + chars * (GLYPH_W + SPACING) - SPACING;
        let rows = PADDING * 2 + 2 * GLYPH_H + LEADING;
        let mut cells = vec![BACKGROUND; cols * rows];
        for (n, line) in lines.iter().enumerate() {
            let value = if n == 0 { TEXT } else { DIM };
            let top = PADDING + n * (GLYPH_H + LEADING);
            for (i, ch) in line.chars().enumerate() {
                let left = PADDING + i * (GLYPH_W + SPACING);
                for (y, bits) in glyph(ch).iter().enumerate() {
                    for x in 0..GLYPH_W {
                        if bits & (0x10 >> x) != 0 {
                            cells[(top + y) * cols + left + x] = value;
                        }
                    }
                }
            }
        }
        let s = scale as usize;
        let (width, height) = (cols * s, rows * s);
        let mut pixels = vec![BACKGROUND; width * height];
        for (y, row) in pixels.chunks_exact_mut(width).enumerate() {
            for (x, px) in row.iter_mut().enumerate() {
                *px = cells[(y / s) * cols + x / s];
            }
        }
        Self { width: width as u32, height: height as u32, scale, pixels }
    }

    /// Horizontal runs of one value, as `(x, y, width, value)`, for renderers that fill
    /// rectangles. One run per cell row is enough since rows repeat `scale` times.
    pub fn runs(&self) -> Vec<(u32, u32, u32, u8)> {
        let mut runs = Vec::new();
        let w = self.width as usize;
        for (y, row) in self.pixels.chunks_exact(w).enumerate().step_by(self.scale as usize) {
            let mut x = 0;
            while x < w {
                let value = row[x];
                let start = x;
                while x < w && row[x] == value {
                    x += 1;
                }
                runs.push((start as u32, y as u32, (x - start) as u32, value));
            }
        }
        runs
    }
}

/// Rows of five bits, most significant bit on the left.
fn glyph(ch: char) -> [u8; GLYPH_H] {
    match ch.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x11, 0x1F, 0x11, 0x11],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        _ => [0; GLYPH_H],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_the_overlay_can_say_has_a_glyph() {
        for vrr in [VrrState::On, VrrState::Off, VrrState::Unsupported, VrrState::Unknown] {
            for cap in [0, 48] {
                let text = Text { fps: 1234567890, cap, vrr };
                for ch in text.lines().concat().chars().filter(|c| *c != ' ') {
                    assert_ne!(glyph(ch), [0; GLYPH_H], "no glyph for {ch:?}");
                }
            }
        }
    }

    #[test]
    fn the_bitmap_scales_and_its_runs_cover_every_row() {
        let text = Text { fps: 48, cap: 48, vrr: VrrState::On };
        let one = Bitmap::render(&text, 1);
        let three = Bitmap::render(&text, 3);
        assert_eq!((three.width, three.height), (one.width * 3, one.height * 3));
        assert_eq!(one.pixels.len(), (one.width * one.height) as usize);
        assert!(one.pixels.contains(&TEXT) && one.pixels.contains(&DIM));
        let covered: u32 = three.runs().iter().map(|r| r.2).sum();
        assert_eq!(covered, three.width * one.height);
        assert_eq!(scale_for(1080), 2);
        assert_eq!(scale_for(2160), 4);
    }
}
