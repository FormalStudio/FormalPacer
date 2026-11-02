//! The limiter. It sleeps before present until the next deadline, keeping deadlines on one
//! cadence so frames leave at even intervals, and records every frame for the app.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use formalpacer_shm::{Mapping, Sample, now_ns};

use crate::game::Game;
use crate::hotkey::Hotkey;
use crate::overlay::{self, Bitmap, Text};

/// A cap the app has not refreshed for this long is dropped: the app was closed or crashed.
const STALE_NS: u64 = 3_000_000_000;
/// The last stretch before a deadline is spun rather than slept, because a timer wakeup can land
/// up to a few tens of microseconds late.
const SPIN_NS: u64 = 250_000;

static X11_WINDOW: AtomicU32 = AtomicU32::new(0);

/// The overlay's frame rate is counted over this long.
const FPS_WINDOW_NS: u64 = 500_000_000;

/// The window of the newest X11 surface; the app asks the X server where it is.
pub fn note_x11_window(window: u32) {
    X11_WINDOW.store(window, Ordering::Relaxed);
}

/// The size of the newest swapchain; the app matches it against display modes when no
/// compositor says where the window is.
pub fn note_extent(width: u32, height: u32) {
    if let Ok(state) = STATE.lock()
        && let Some(page) = &state.page
    {
        page.set_swap_extent(width, height);
    }
    if let Ok(mut extent) = EXTENT.lock() {
        *extent = (width, height);
    }
}

static EXTENT: Mutex<(u32, u32)> = Mutex::new((0, 0));

/// The overlay picture, and a number that changes whenever the picture does.
#[derive(Clone)]
pub struct Frame {
    pub generation: u64,
    pub bitmap: Arc<Bitmap>,
}

struct State {
    page: Option<Mapping>,
    page_failed: bool,
    last_mark: u64,
    deadline: u64,
    hotkey: Hotkey,
    fps_since: u64,
    fps_frames: u32,
    fps: u32,
    frame: Option<(Text, Frame)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    page: None,
    page_failed: false,
    last_mark: 0,
    deadline: 0,
    hotkey: Hotkey::new(),
    fps_since: 0,
    fps_frames: 0,
    fps: 0,
    frame: None,
});

/// What to draw over a swapchain `swap_height` pixels tall this frame, if the overlay is on.
pub fn overlay(swap_height: u32) -> Option<Frame> {
    let mut state = STATE.lock().ok()?;
    let page = state.page.as_ref()?;
    let wanted = page.overlay();
    if !wanted.show {
        return None;
    }
    let target = page.target(now_ns(), STALE_NS);
    let text = Text {
        fps: state.fps,
        cap: if target > 0 { (1e9 / target as f64).round() as u32 } else { 0 },
        vrr: wanted.vrr,
    };
    let scale = overlay::scale_for(swap_height);
    if let Some((shown, frame)) = &state.frame
        && *shown == text
        && frame.bitmap.scale == scale
    {
        return Some(frame.clone());
    }
    let generation = state.frame.as_ref().map_or(1, |(_, f)| f.generation + 1);
    let frame = Frame { generation, bitmap: Arc::new(Bitmap::render(&text, scale)) };
    state.frame = Some((text, frame.clone()));
    Some(frame)
}

pub fn before_present(game: &Game) {
    let Ok(mut state) = STATE.lock() else { return };
    if state.page.is_none() && !state.page_failed {
        match Mapping::create(&game.name, game.steam_app_id) {
            Ok(page) => {
                if let Ok(extent) = EXTENT.lock() {
                    page.set_swap_extent(extent.0, extent.1);
                }
                state.page = Some(page);
                // SAFETY: registers a plain extern "C" function.
                unsafe { libc::atexit(unlink_at_exit) };
            }
            Err(_) => state.page_failed = true,
        }
    }
    if let Some(page) = &state.page {
        page.set_x11_window(X11_WINDOW.load(Ordering::Relaxed));
    }
    let now = now_ns();
    let target = state.page.as_ref().map_or(0, |p| p.target(now, STALE_NS));
    let mut mark = now;
    if target > 0 {
        let deadline = state.deadline;
        // A deadline more than two frames out means the cap just got tighter: start over.
        if deadline > now && deadline - now <= 2 * target {
            sleep_until(deadline);
            mark = now_ns();
        }
        let on_cadence = deadline != 0 && mark + 2 * target >= deadline && mark <= deadline + target / 2;
        state.deadline = if on_cadence { deadline + target } else { mark + target };
    } else {
        state.deadline = 0;
    }
    if state.hotkey.pressed(mark)
        && let Some(page) = &state.page
    {
        page.request_toggle();
    }
    state.fps_frames += 1;
    if mark.saturating_sub(state.fps_since) >= FPS_WINDOW_NS {
        if state.fps_since != 0 {
            state.fps = (f64::from(state.fps_frames) * 1e9 / (mark - state.fps_since) as f64).round() as u32;
        }
        state.fps_since = mark;
        state.fps_frames = 0;
    }
    if state.last_mark != 0
        && let Some(page) = &state.page
    {
        let us = |ns: u64| (ns / 1000).min(u64::from(u32::MAX)) as u32;
        page.push(Sample { interval_us: us(mark - state.last_mark), slept_us: us(mark - now) }, mark);
    }
    state.last_mark = mark;
}

fn sleep_until(deadline: u64) {
    if deadline > now_ns() + SPIN_NS {
        let wake = deadline - SPIN_NS;
        let ts = libc::timespec {
            tv_sec: (wake / 1_000_000_000) as libc::time_t,
            tv_nsec: (wake % 1_000_000_000) as libc::c_long,
        };
        // SAFETY: absolute sleep on the monotonic clock; an early EINTR return is covered by the spin.
        unsafe { libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &ts, std::ptr::null_mut()) };
    }
    while now_ns() < deadline {
        std::hint::spin_loop();
    }
}

extern "C" fn unlink_at_exit() {
    if let Ok(state) = STATE.lock()
        && let Some(page) = &state.page
    {
        page.unlink();
    }
}
