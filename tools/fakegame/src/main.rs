//! Vulkan window that clears to a cycling colour at a configurable simulated
//! CPU cost per frame, for observing an external frame limiter.

mod gfx;
#[cfg(windows)]
mod native;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use gfx::Gfx;

struct Args {
    present: gfx::Present,
    fps: f64,
    jitter_pct: f64,
    spike_every: u64,
    spike_ms: f64,
    seconds: Option<f64>,
    fullscreen: bool,
    background: bool,
    /// Present through Direct3D or OpenGL instead of Vulkan.
    #[cfg(windows)]
    api: Option<native::Api>,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        present: gfx::Present::Immediate,
        fps: 0.0,
        jitter_pct: 5.0,
        spike_every: 0,
        spike_ms: 0.0,
        seconds: None,
        fullscreen: false,
        background: false,
        #[cfg(windows)]
        api: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut val = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--present" => {
                a.present = match val()?.as_str() {
                    "immediate" => gfx::Present::Immediate,
                    "mailbox" => gfx::Present::Mailbox,
                    "fifo" => gfx::Present::Fifo,
                    other => bail!("unknown present mode {other}"),
                }
            }
            "--fps" => a.fps = val()?.parse()?,
            "--jitter" => a.jitter_pct = val()?.parse()?,
            "--spike-every" => a.spike_every = val()?.parse()?,
            "--spike-ms" => a.spike_ms = val()?.parse()?,
            "--seconds" => a.seconds = Some(val()?.parse()?),
            "--fullscreen" => a.fullscreen = true,
            "--background" => a.background = true,
            #[cfg(windows)]
            "--api" => {
                a.api = match val()?.as_str() {
                    "vulkan" => None,
                    "d3d11" => Some(native::Api::D3d11),
                    "d3d12" => Some(native::Api::D3d12),
                    "gl" => Some(native::Api::Gl),
                    other => bail!("unknown api {other}"),
                }
            }
            other => bail!(
                "unknown argument {other}\nusage: fakegame [--present immediate|mailbox|fifo] \
                 [--fps N] [--jitter P] [--spike-every K --spike-ms M] [--seconds S] [--fullscreen] [--background] \
                 [--api vulkan|d3d11|d3d12|gl]"
            ),
        }
    }
    Ok(a)
}

/// xorshift64, enough for load jitter.
struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn spin(d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        std::hint::spin_loop();
    }
}

/// Frame intervals between successive present calls, reported once a second.
struct Stats {
    last_present: Option<Instant>,
    window_start: Instant,
    intervals_ms: Vec<f64>,
}

impl Stats {
    fn on_present(&mut self, now: Instant) {
        if let Some(prev) = self.last_present {
            self.intervals_ms.push((now - prev).as_secs_f64() * 1000.0);
        }
        self.last_present = Some(now);
        let elapsed = (now - self.window_start).as_secs_f64();
        if elapsed >= 1.0 && !self.intervals_ms.is_empty() {
            let fps = self.intervals_ms.len() as f64 / elapsed;
            self.intervals_ms.sort_by(f64::total_cmp);
            let idx = ((self.intervals_ms.len() as f64 * 0.99).ceil() as usize)
                .saturating_sub(1)
                .min(self.intervals_ms.len() - 1);
            eprintln!("fakegame: fps={fps:.1} p99_ms={:.2}", self.intervals_ms[idx]);
            self.intervals_ms.clear();
            self.window_start = now;
        }
    }
}

struct App {
    args: Args,
    // Declared before `window`: the surface must be destroyed before the window.
    gfx: Option<Gfx>,
    #[cfg(windows)]
    native: Option<native::Native>,
    window: Option<Window>,
    rng: Rng,
    stats: Stats,
    start: Instant,
    frame: u64,
    resized: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn finish(&mut self, event_loop: &ActiveEventLoop, err: Option<anyhow::Error>) {
        self.gfx = None;
        #[cfg(windows)]
        {
            self.native = None;
        }
        self.window = None;
        if err.is_some() {
            self.error = err;
        }
        event_loop.exit();
    }

    /// Simulated game work, and the colour of the frame.
    fn work(&mut self) -> [f32; 4] {
        self.frame += 1;
        let mut work = if self.args.fps > 0.0 {
            let j = (self.rng.unit() * 2.0 - 1.0) * self.args.jitter_pct / 100.0;
            (1.0 / self.args.fps) * (1.0 + j)
        } else {
            0.0
        };
        if self.args.spike_every > 0 && self.frame.is_multiple_of(self.args.spike_every) {
            work += self.args.spike_ms / 1000.0;
        }
        spin(Duration::from_secs_f64(work.max(0.0)));
        let t = self.start.elapsed().as_secs_f32();
        let phase = |o: f32| 0.5 + 0.5 * (t * 0.8 + o).sin();
        self.stats.on_present(Instant::now());
        [phase(0.0), phase(2.1), phase(4.2), 1.0]
    }

    fn frame(&mut self) -> Result<()> {
        let Some(size) = self.window.as_ref().map(Window::inner_size) else { return Ok(()) };
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        #[cfg(windows)]
        if self.native.is_some() {
            let color = self.work();
            if let Some(native) = self.native.as_mut() {
                native.present(size.width, size.height, color)?;
            }
            return Ok(());
        }
        let Some(gfx) = self.gfx.as_mut() else { return Ok(()) };
        if self.resized {
            gfx.recreate(size.width, size.height)?;
            self.resized = false;
        }
        let Some(frame) = gfx.begin_frame(size.width, size.height)? else {
            return Ok(());
        };
        // Between acquire and submit, like a game.
        let color = self.work();
        if let Some(gfx) = self.gfx.as_mut() {
            self.resized |= gfx.end_frame(frame, color)?;
        }
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let result = (|| {
            let mut attributes = Window::default_attributes()
                .with_title("fakegame")
                .with_fullscreen(self.args.fullscreen.then_some(winit::window::Fullscreen::Borderless(None)));
            if self.args.background {
                // Never focused, and all but a few pixels past the primary monitor's bottom-right
                // corner, for runs on a machine someone is using. Fully off the desktop, the
                // window is never composed, and frame tracing sees no presents.
                let (w, h) = event_loop.primary_monitor().map_or((1920, 1080), |m| (m.size().width, m.size().height));
                attributes = attributes
                    .with_active(false)
                    .with_decorations(false)
                    .with_position(winit::dpi::PhysicalPosition::new(w.saturating_sub(4), h.saturating_sub(4)));
            }
            let window = event_loop.create_window(attributes).context("create window")?;
            let size = window.inner_size();
            #[cfg(windows)]
            if let Some(api) = self.args.api {
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
                    bail!("not a Win32 window")
                };
                self.native = Some(native::Native::new(api, handle.hwnd, size.width, size.height)?);
                return Ok((window, None));
            }
            let gfx = Gfx::new(&window, self.args.present, size.width, size.height)?;
            Ok::<_, anyhow::Error>((window, Some(gfx)))
        })();
        match result {
            Ok((window, gfx)) => {
                self.window = Some(window);
                self.gfx = gfx;
                self.start = Instant::now();
                self.stats.window_start = self.start;
            }
            Err(e) => self.finish(event_loop, Some(e)),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.finish(event_loop, None),
            WindowEvent::Resized(_) => self.resized = true,
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(s) = self.args.seconds
            && self.start.elapsed().as_secs_f64() >= s
        {
            return self.finish(event_loop, None);
        }
        if let Err(e) = self.frame() {
            self.finish(event_loop, Some(e));
        }
    }
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let now = Instant::now();
    let mut app = App {
        args,
        gfx: None,
        #[cfg(windows)]
        native: None,
        window: None,
        rng: Rng(0x9E37_79B9_7F4A_7C15),
        stats: Stats { last_present: None, window_start: now, intervals_ms: Vec::new() },
        start: now,
        frame: 0,
        resized: false,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}
