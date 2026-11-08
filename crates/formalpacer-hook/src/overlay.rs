//! The overlay's picture: two or three lines of text in a 5x7 pixel font on a dark box, and a
//! graph of the newest frame times under them, as one byte per pixel. The Vulkan layer copies it into the swapchain image and the OpenGL hook clears it in
//! runs, so neither needs shaders, a font atlas or blending.

use formalpacer_shm::VrrState;

pub const BACKGROUND: u8 = 0;
pub const TEXT: u8 = 1;
pub const DIM: u8 = 2;
/// A frame in the graph that left on time.
pub const STEADY: u8 = 3;
/// A frame in the graph that took well over its interval.
pub const SPIKE: u8 = 4;

const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;
/// Cells between glyphs, between lines and around the text.
const SPACING: usize = 1;
const LEADING: usize = 3;
const PADDING: usize = 3;

/// Gap from the top-left corner of the swapchain, in cells.
pub const MARGIN: u32 = 4;

/// RGBA for each pixel value.
pub const COLORS: [[u8; 4]; 5] =
    [[16, 16, 18, 255], [242, 242, 242, 255], [150, 150, 156, 255], [96, 208, 120, 255], [240, 84, 72, 255]];

/// Frames the graph shows, one cell column each.
pub const GRAPH_FRAMES: usize = 90;
/// Odd, so the frame interval the graph is centred on has a row of its own.
const GRAPH_H: usize = 15;
/// A frame this many times its interval or longer shows as a spike.
const SPIKE_AT: f64 = 1.25;

/// What the overlay says.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub fps: u32,
    /// The cap in fps, 0 for none.
    pub cap: u32,
    /// The cap the game's next launch gets, 0 when it is the one it has now. Only a cap the driver
    /// holds differs between runs.
    pub next: u32,
    pub vrr: VrrState,
    /// The display's refresh rate and the bottom of its VRR window, 0 when not known.
    pub refresh_hz: u32,
    pub vrr_min_hz: u32,
    /// The newest frame intervals, oldest first, up to [`GRAPH_FRAMES`]; none, no graph.
    pub frames_us: Vec<u32>,
}

impl Text {
    /// What the display refreshes at. With VRR running it refreshes once per frame, a frame under
    /// the bottom of the VRR window as often as it takes to stay inside it (the driver's low
    /// framerate compensation), and never past the refresh rate.
    pub fn hz(&self) -> u32 {
        match self.vrr {
            VrrState::On if self.fps > 0 => {
                let repeats = self.vrr_min_hz.div_ceil(self.fps).max(1);
                let hz = self.fps * repeats;
                if self.refresh_hz > 0 { hz.min(self.refresh_hz) } else { hz }
            }
            _ => self.refresh_hz,
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let hz = self.hz();
        let vrr = match (self.vrr, hz) {
            (VrrState::Unknown, _) => String::new(),
            (VrrState::On, 0) => "VRR ON".to_owned(),
            (VrrState::Off, 0) => "VRR OFF".to_owned(),
            (VrrState::Unsupported, 0) => "NO VRR".to_owned(),
            (VrrState::On, hz) => format!("VRR {hz}HZ"),
            (VrrState::Off, hz) => format!("VRR OFF {hz}HZ"),
            (VrrState::Unsupported, hz) => format!("{hz}HZ"),
        };
        let cap = if self.cap > 0 { format!("CAP {}", self.cap) } else { "UNCAPPED".to_owned() };
        let second = if vrr.is_empty() { cap } else { format!("{cap}  {vrr}") };
        let mut lines = vec![format!("{} FPS", self.fps), second];
        if self.next > 0 {
            lines.push(format!("RELAUNCH FOR CAP {}", self.next));
        }
        lines
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
        let graph = (!text.frames_us.is_empty()).then(|| plot(&text.frames_us, text.cap));
        let cols =
            PADDING * 2 + (chars * (GLYPH_W + SPACING) - SPACING).max(graph.as_ref().map_or(0, |_| GRAPH_FRAMES));
        let text_rows = lines.len() * (GLYPH_H + LEADING) - LEADING;
        let rows = PADDING * 2 + text_rows + graph.as_ref().map_or(0, |_| LEADING + GRAPH_H);
        let mut cells = vec![BACKGROUND; cols * rows];
        if let Some(graph) = graph {
            let top = PADDING + text_rows + LEADING;
            for (y, row) in graph.chunks_exact(GRAPH_FRAMES).enumerate() {
                cells[(top + y) * cols + PADDING..][..GRAPH_FRAMES].copy_from_slice(row);
            }
        }
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

/// The graph's cells, [`GRAPH_FRAMES`] wide and [`GRAPH_H`] tall, the newest frame on the right
/// and longer frames higher up. The middle row, dotted, is the cap's interval (the usual one
/// without a cap), so a game paced evenly draws a flat line along it.
fn plot(frames_us: &[u32], cap: u32) -> Vec<u8> {
    let mut cells = vec![BACKGROUND; GRAPH_FRAMES * GRAPH_H];
    let shown = &frames_us[frames_us.len().saturating_sub(GRAPH_FRAMES)..];
    let interval = if cap > 0 {
        1e6 / f64::from(cap)
    } else {
        let mut sorted = shown.to_vec();
        sorted.sort_unstable();
        f64::from(sorted[sorted.len() / 2])
    };
    if interval <= 0. {
        return cells;
    }
    let top = GRAPH_H - 1;
    let row = |us: u32| top - ((f64::from(us) / (2. * interval) * top as f64).round() as usize).min(top);
    let middle = row(interval.round() as u32);
    for x in (0..GRAPH_FRAMES).step_by(2) {
        cells[middle * GRAPH_FRAMES + x] = DIM;
    }
    let spike = |us: u32| f64::from(us) >= interval * SPIKE_AT;
    let left = GRAPH_FRAMES - shown.len();
    let mut prev: Option<u32> = None;
    for (i, &us) in shown.iter().enumerate() {
        let (a, b) = (row(us), prev.map_or(row(us), row));
        let value = if spike(us) || prev.is_some_and(spike) { SPIKE } else { STEADY };
        for y in a.min(b)..=a.max(b) {
            cells[y * GRAPH_FRAMES + left + i] = value;
        }
        prev = Some(us);
    }
    cells
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
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        _ => [0; GLYPH_H],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_the_overlay_can_say_has_a_glyph() {
        for vrr in [VrrState::On, VrrState::Off, VrrState::Unsupported, VrrState::Unknown] {
            for (cap, refresh_hz) in [(0, 0), (48, 144)] {
                let text =
                    Text { fps: 1234567890, cap, next: 1234, vrr, refresh_hz, vrr_min_hz: 48, frames_us: Vec::new() };
                for ch in text.lines().concat().chars().filter(|c| *c != ' ') {
                    assert_ne!(glyph(ch), [0; GLYPH_H], "no glyph for {ch:?}");
                }
            }
        }
    }

    #[test]
    fn the_bitmap_scales_and_its_runs_cover_every_row() {
        let text = Text {
            fps: 48,
            cap: 48,
            next: 0,
            vrr: VrrState::On,
            refresh_hz: 144,
            vrr_min_hz: 48,
            frames_us: Vec::new(),
        };
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

    #[test]
    fn the_graph_draws_even_frames_flat_and_a_long_one_as_a_spike() {
        let mut frames = vec![10_000; 40];
        frames[30] = 25_000;
        let cells = plot(&frames, 100);
        let column = |x: usize| (0..GRAPH_H).filter(|&y| cells[y * GRAPH_FRAMES + x] >= STEADY).collect::<Vec<_>>();
        let middle = GRAPH_H / 2;
        let first = GRAPH_FRAMES - frames.len();
        assert!(column(first - 1).is_empty());
        assert_eq!(column(first + 1), [middle]);
        assert_eq!(column(first + 30), (0..=middle).collect::<Vec<_>>());
        assert_eq!(cells[first + 30], SPIKE);
        assert_eq!(cells[middle * GRAPH_FRAMES + first + 1], STEADY);
        let text =
            Text { fps: 100, cap: 100, next: 0, vrr: VrrState::On, refresh_hz: 144, vrr_min_hz: 48, frames_us: frames };
        let bitmap = Bitmap::render(&text, 1);
        assert!(bitmap.pixels.contains(&SPIKE) && bitmap.pixels.contains(&STEADY));
    }

    #[test]
    fn the_refresh_rate_follows_the_frame_rate_only_while_vrr_runs() {
        let at = |fps, vrr| Text { fps, cap: 0, next: 0, vrr, refresh_hz: 144, vrr_min_hz: 48, frames_us: Vec::new() };
        assert_eq!(at(115, VrrState::On).hz(), 115);
        assert_eq!(at(115, VrrState::On).lines()[1], "UNCAPPED  VRR 115HZ");
        // Under the window each frame is shown twice, or more.
        assert_eq!(at(40, VrrState::On).hz(), 80);
        assert_eq!(at(20, VrrState::On).hz(), 60);
        assert_eq!(at(300, VrrState::On).hz(), 144);
        assert_eq!(at(115, VrrState::Off).lines()[1], "UNCAPPED  VRR OFF 144HZ");
        assert_eq!(at(115, VrrState::Unsupported).hz(), 144);
    }
}
