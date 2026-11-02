//! The limiter. It sleeps before present until the next deadline, keeping deadlines on one
//! cadence so frames leave at even intervals, and records every frame for the app.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use formalpacer_shm::{Mapping, Sample, now_ns};

use crate::game::Game;

/// A cap the app has not refreshed for this long is dropped: the app was closed or crashed.
const STALE_NS: u64 = 3_000_000_000;
/// The last stretch before a deadline is spun rather than slept, because a timer wakeup can land
/// up to a few tens of microseconds late.
const SPIN_NS: u64 = 250_000;

static X11_WINDOW: AtomicU32 = AtomicU32::new(0);

/// The window of the newest X11 surface; the app asks the X server where it is.
pub fn note_x11_window(window: u32) {
    X11_WINDOW.store(window, Ordering::Relaxed);
}

struct State {
    page: Option<Mapping>,
    page_failed: bool,
    last_mark: u64,
    deadline: u64,
}

static STATE: Mutex<State> = Mutex::new(State { page: None, page_failed: false, last_mark: 0, deadline: 0 });

pub fn before_present(game: &Game) {
    let Ok(mut state) = STATE.lock() else { return };
    if state.page.is_none() && !state.page_failed {
        match Mapping::create(&game.name, game.steam_app_id) {
            Ok(page) => {
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
