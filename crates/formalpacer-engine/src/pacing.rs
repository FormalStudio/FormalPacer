//! Choosing the frame rate to hold.
//!
//! The layer reports, per frame, how long the game needed (`work`, the interval minus the time
//! the layer slept). The 95th percentile of that over the last two seconds is what the game can
//! sustain. The plan keeps 8% of it in reserve so ordinary scene changes never touch the cap,
//! then picks:
//!
//! - Fixed refresh: the largest whole fraction of the refresh rate the game can hold (144, 72,
//!   48, 36 on a 144 Hz panel). Every frame then stays on screen for the same number of
//!   refreshes, which is the console 30/40/60 rule.
//! - VRR on: anything inside the panel's VRR window is smooth, so the plan holds just under the
//!   refresh rate when the game is fast (`r - r²/3600`, which keeps frames inside the window
//!   instead of hitting the vsync wall), and otherwise the highest steady rate in 5 fps steps,
//!   snapped to a fraction of the refresh rate when that costs at most 10%.
//! - Never below the VRR window while VRR is on: under it the driver shows each frame twice or
//!   more, which many panels show as flicker. A game that can just about reach the bottom of the
//!   window is held there with less headroom; one that cannot gets no cap, since no cap helps.
//!
//! The cap drops at once when the game can no longer hold it, and rises only after the game has
//! held the higher rate for five seconds, so it does not hunt.
//!
//! Work under a cap can lie. An engine that builds the next frame on other threads while the
//! layer holds the present (BeamNG, most big engines) shows almost no work per frame, so it looks
//! able to run far faster than it can. A raise that has to be taken back within ten seconds marks
//! the game as such: from then on the cap rises one step at a time, and after a failed step it
//! waits 30 seconds before trying again, doubling up to five minutes.

use formalpacer_shm::Sample;

use crate::display::Display;

const HEADROOM: f64 = 0.92;
/// Lower the cap when the game sustains less than the cap over this factor.
const LOWER_AT: f64 = 0.97;
const RAISE_AFTER_S: f64 = 5.0;
/// A cap lowered this soon after it was raised was a raise the game could not hold.
const FAILED_WITHIN_S: f64 = 10.0;
const BACKOFF_START_S: f64 = 30.0;
const BACKOFF_MAX_S: f64 = 300.0;
const WINDOW_S: f64 = 2.0;
const MIN_FPS: f64 = 20.0;
/// Frames longer than this are loading hitches or alt-tabs, not the game's pace.
const HITCH_US: u32 = 250_000;
const VRR_FALLBACK_MIN: f64 = 48.0;
/// Snap to a refresh fraction under VRR only when it costs at most this share of the free rate.
const SNAP_TOLERANCE: f64 = 0.90;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CapKind {
    /// Just under the refresh rate with VRR on.
    VrrCeiling,
    /// A steady rate inside the VRR window.
    VrrSteady,
    /// The refresh rate divided by `n`.
    Fraction(u32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cap {
    pub fps: f64,
    pub kind: CapKind,
}

impl Cap {
    pub fn interval_ns(&self) -> u64 {
        (1e9 / self.fps).round() as u64
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub avg_fps: f64,
    /// The 99th percentile frame interval as a rate (the usual "1% low").
    pub low_fps: f64,
    /// What the game could run at with no cap, from the 95th percentile of its work.
    pub capacity_fps: f64,
    pub frames: usize,
    pub span_s: f64,
}

/// Stats over the newest `WINDOW_S` seconds of `samples` (oldest first).
pub fn stats(samples: &[Sample]) -> Stats {
    let mut intervals = Vec::new();
    let mut works = Vec::new();
    let mut span = 0u64;
    for s in samples.iter().rev() {
        if s.interval_us == 0 || s.interval_us > HITCH_US {
            continue;
        }
        span += u64::from(s.interval_us);
        intervals.push(s.interval_us);
        works.push(s.work_us().max(1));
        if span as f64 >= WINDOW_S * 1e6 {
            break;
        }
    }
    if intervals.is_empty() {
        return Stats::default();
    }
    let span_s = span as f64 / 1e6;
    Stats {
        avg_fps: intervals.len() as f64 / span_s,
        low_fps: 1e6 / f64::from(percentile(&mut intervals, 0.99)),
        capacity_fps: 1e6 / f64::from(percentile(&mut works, 0.95)),
        frames: intervals.len(),
        span_s,
    }
}

fn percentile(values: &mut [u32], p: f64) -> u32 {
    values.sort_unstable();
    let i = ((values.len() - 1) as f64 * p).round() as usize;
    values[i]
}

/// What the game can sustain. A game locked to the refresh rate by vsync spends the rest of each
/// frame blocked in present, which reads as work; it can do at least the refresh rate.
pub fn capacity(stats: &Stats, refresh_hz: f64) -> f64 {
    let vsync_locked =
        (stats.avg_fps - refresh_hz).abs() <= refresh_hz * 0.01 && stats.capacity_fps >= refresh_hz * 0.95;
    if vsync_locked { stats.capacity_fps.max(refresh_hz / HEADROOM) } else { stats.capacity_fps }
}

/// The best cap for a game that sustains `capacity_fps` on `display`. `None` when the game is
/// too slow for any cap to help.
pub fn plan(capacity_fps: f64, display: &Display) -> Option<Cap> {
    plan_budget(capacity_fps * HEADROOM, display)
}

/// The best cap at or under `budget` fps.
fn plan_budget(budget: f64, display: &Display) -> Option<Cap> {
    let refresh = display.refresh_hz;
    if display.vrr_active() {
        let (vrr_min, vrr_max) = display.vrr_range.unwrap_or((VRR_FALLBACK_MIN, refresh));
        let ceiling = (refresh.min(vrr_max) - refresh * refresh / 3600.).floor();
        if budget >= ceiling {
            return Some(Cap { fps: ceiling, kind: CapKind::VrrCeiling });
        }
        let floor = vrr_min.ceil().max(MIN_FPS);
        if budget >= floor {
            let steady = ((budget / 5.).floor() * 5.).max(floor);
            let snapped = fraction(refresh, steady).filter(|c| c.fps >= steady * SNAP_TOLERANCE && c.fps >= floor);
            return Some(snapped.unwrap_or(Cap { fps: steady, kind: CapKind::VrrSteady }));
        }
        return (budget / HEADROOM >= floor).then_some(Cap { fps: floor, kind: CapKind::VrrSteady });
    }
    fraction(refresh, budget)
}

/// The largest `refresh / n` at or under `budget`, no lower than `MIN_FPS`.
fn fraction(refresh: f64, budget: f64) -> Option<Cap> {
    (1..=8u32)
        .map(|n| Cap { fps: refresh / f64::from(n), kind: CapKind::Fraction(n) })
        .find(|c| c.fps <= budget + 1e-6 && c.fps >= MIN_FPS)
}

/// The next cap above `current` that [`plan`] could pick.
fn next_step(current: Cap, display: &Display) -> Option<Cap> {
    let top = display.refresh_hz.ceil() as u32 + 1;
    (current.fps.floor() as u32 + 1..=top)
        .filter_map(|budget| plan_budget(f64::from(budget), display))
        .find(|c| c.fps > current.fps + 0.01)
}

/// One game's cap over time, with the hysteresis described above.
#[derive(Debug, Default)]
pub struct Pacer {
    pub cap: Option<Cap>,
    higher_since: Option<f64>,
    short_ticks: u32,
    /// When the cap last went up, until that raise has held for `FAILED_WITHIN_S`.
    raised_at: Option<f64>,
    /// A raise failed: the game's work under a cap is not to be trusted.
    stepping: bool,
    /// No raise before this time, after a failed one.
    blocked_until: f64,
    backoff_s: f64,
}

impl Pacer {
    /// Feeds the newest stats at time `now_s`; returns the cap to hold.
    pub fn update(&mut self, stats: &Stats, display: &Display, now_s: f64) -> Option<Cap> {
        // Too little data, or a loading screen: hold what we have.
        if stats.span_s < 1.0 || stats.avg_fps < 10. {
            return self.cap;
        }
        let capacity = capacity(stats, display.refresh_hz);
        let wanted = plan(capacity, display);
        let Some(current) = self.cap else {
            self.cap = wanted;
            return self.cap;
        };
        let wanted_fps = wanted.map_or(0., |c| c.fps);
        if self.raised_at.is_some_and(|t| now_s - t > FAILED_WITHIN_S) {
            self.raised_at = None;
            self.backoff_s = BACKOFF_START_S;
        }
        if !self.fits(current, display) {
            self.adopt(wanted);
        } else if capacity * LOWER_AT < current.fps {
            self.short_ticks += 1;
            if self.short_ticks >= 2 {
                if self.raised_at.take().is_some() {
                    self.stepping = true;
                    let backoff = if self.backoff_s > 0. { self.backoff_s } else { BACKOFF_START_S };
                    self.blocked_until = now_s + backoff;
                    self.backoff_s = (backoff * 2.).min(BACKOFF_MAX_S);
                }
                self.adopt(wanted);
            }
        } else if wanted_fps > current.fps + 0.01 && now_s >= self.blocked_until {
            self.short_ticks = 0;
            let since = *self.higher_since.get_or_insert(now_s);
            if now_s - since >= RAISE_AFTER_S {
                let raise = if self.stepping { next_step(current, display) } else { wanted };
                self.adopt(raise);
                self.raised_at = Some(now_s);
            }
        } else {
            self.short_ticks = 0;
            self.higher_since = None;
        }
        self.cap
    }

    /// Whether `cap` still suits the display: a VRR cap needs VRR on, a fraction needs the
    /// refresh rate it was a fraction of.
    fn fits(&self, cap: Cap, display: &Display) -> bool {
        match cap.kind {
            CapKind::VrrCeiling | CapKind::VrrSteady => display.vrr_active(),
            CapKind::Fraction(_) => display.vrr_active() || is_fraction_of(cap.fps, display.refresh_hz),
        }
    }

    fn adopt(&mut self, cap: Option<Cap>) {
        self.cap = cap;
        self.higher_since = None;
        self.short_ticks = 0;
    }
}

fn is_fraction_of(fps: f64, refresh: f64) -> bool {
    (1..=8).any(|n| (refresh / f64::from(n) - fps).abs() < 0.01)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Vrr;

    fn panel(refresh: f64, vrr: Vrr) -> Display {
        Display {
            connector: "DP-1".into(),
            model: None,
            width: 2560,
            height: 1440,
            refresh_hz: refresh,
            max_refresh_hz: refresh,
            vrr,
            vrr_range: Some((48., refresh)),
        }
    }

    fn fps(cap: Option<Cap>) -> f64 {
        cap.map_or(0., |c| (c.fps * 100.).round() / 100.)
    }

    #[test]
    fn fixed_refresh_uses_whole_fractions() {
        let p = panel(144., Vrr::Unsupported);
        assert_eq!(fps(plan(200., &p)), 144.);
        assert_eq!(fps(plan(60., &p)), 48.);
        assert_eq!(fps(plan(100., &p)), 72.);
        assert_eq!(fps(plan(45., &p)), 36.);
        assert_eq!(fps(plan(15., &p)), 0.);
        let p = panel(60., Vrr::Off);
        assert_eq!(fps(plan(58., &p)), 30.);
        assert_eq!(plan(58., &p).map(|c| c.kind), Some(CapKind::Fraction(2)));
    }

    #[test]
    fn vrr_stays_inside_the_window() {
        let p = panel(144., Vrr::Active);
        assert_eq!(plan(200., &p), Some(Cap { fps: 138., kind: CapKind::VrrCeiling }));
        assert_eq!(fps(plan(100., &p)), 90.);
        // 55 would be the steady rate; 48 is a third of 144 but costs more than 10%.
        assert_eq!(fps(plan(60., &p)), 55.);
        // 75 is the steady rate and 72 is half of 144 within 10%: the fraction wins.
        assert_eq!(plan(84., &p), Some(Cap { fps: 72., kind: CapKind::Fraction(2) }));
        // Never under the 48 Hz floor: a game that reaches 48 is held there with less headroom,
        // one that cannot is left alone.
        assert_eq!(fps(plan(50., &p)), 48.);
        assert_eq!(fps(plan(45., &p)), 0.);
        // On a 30-144 panel, 30 is inside the window; 28.8 (a fifth of 144) and 24 are not.
        let mut wide = panel(144., Vrr::Active);
        wide.vrr_range = Some((30., 144.));
        assert_eq!(fps(plan(35., &wide)), 30.);
        assert_eq!(fps(plan(31., &wide)), 30.);
        assert_eq!(fps(plan(28., &wide)), 0.);
        assert_eq!(fps(plan(300., &panel(240., Vrr::Active))), 224.);
    }

    #[test]
    fn exact_refresh_fraction() {
        let p = panel(143.973, Vrr::Unsupported);
        assert_eq!(fps(plan(60., &p)), 47.99);
    }

    fn steady(capacity: f64) -> Stats {
        Stats { avg_fps: capacity, low_fps: capacity, capacity_fps: capacity, frames: 200, span_s: 2. }
    }

    #[test]
    fn drops_fast_and_rises_slow() {
        let p = panel(144., Vrr::Unsupported);
        let mut pacer = Pacer::default();
        assert_eq!(fps(pacer.update(&steady(160.), &p, 0.)), 144.);
        // One short tick is a blip, two are a trend.
        assert_eq!(fps(pacer.update(&steady(100.), &p, 0.25)), 144.);
        assert_eq!(fps(pacer.update(&steady(100.), &p, 0.5)), 72.);
        // A brief recovery is not enough to go back up.
        assert_eq!(fps(pacer.update(&steady(170.), &p, 1.)), 72.);
        assert_eq!(fps(pacer.update(&steady(170.), &p, 5.)), 72.);
        assert_eq!(fps(pacer.update(&steady(170.), &p, 6.1)), 144.);
        // Small wobble under the headroom does not move the cap.
        assert_eq!(fps(pacer.update(&steady(149.), &p, 7.)), 144.);
        assert_eq!(fps(pacer.update(&steady(149.), &p, 7.5)), 144.);
    }

    #[test]
    fn a_game_whose_work_lies_under_a_cap_climbs_one_step_at_a_time() {
        let mut p = panel(144., Vrr::Active);
        p.vrr_range = Some((30., 144.));
        let mut pacer = Pacer::default();
        // Uncapped, the game really runs at 40.
        assert_eq!(fps(pacer.update(&steady(40.), &p, 0.)), 35.);
        // Capped, its other threads work through the layer's sleep: it looks 50 times faster.
        let looks_fast = steady(2000.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 1.)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 6.1)), 138.);
        // It cannot hold that: back down, and no new try for 30 s however fast it looks.
        assert_eq!(fps(pacer.update(&steady(40.), &p, 6.5)), 138.);
        assert_eq!(fps(pacer.update(&steady(40.), &p, 6.75)), 35.);
        for t in [7., 20., 35.7] {
            assert_eq!(fps(pacer.update(&looks_fast, &p, t)), 35., "at {t}");
        }
        // Then, after the usual five seconds, one step up rather than back to 138.
        assert_eq!(fps(pacer.update(&looks_fast, &p, 37.)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 42.1)), 36.);
        // 36 holds; the next step, 45, does not: down again, and the next try waits 60 s.
        assert_eq!(fps(pacer.update(&looks_fast, &p, 43.)), 36.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 48.1)), 45.);
        assert_eq!(fps(pacer.update(&steady(40.), &p, 48.3)), 45.);
        assert_eq!(fps(pacer.update(&steady(40.), &p, 48.55)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 100.)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 109.)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 114.1)), 36.);
    }

    #[test]
    fn vsync_locked_game_keeps_the_full_rate() {
        let p = panel(144., Vrr::Unsupported);
        let mut pacer = Pacer::default();
        // Blocked in present for vsync: work reads as 140 fps while the game holds 144.
        let locked = Stats { avg_fps: 143.8, low_fps: 140., capacity_fps: 140., frames: 288, span_s: 2. };
        assert_eq!(fps(pacer.update(&locked, &p, 0.)), 144.);
        assert_eq!(fps(pacer.update(&locked, &p, 0.25)), 144.);
        assert_eq!(fps(pacer.update(&locked, &p, 0.5)), 144.);
    }

    #[test]
    fn follows_vrr_switching_off() {
        let mut pacer = Pacer::default();
        assert_eq!(fps(pacer.update(&steady(100.), &panel(144., Vrr::Active), 0.)), 90.);
        assert_eq!(fps(pacer.update(&steady(100.), &panel(144., Vrr::Off), 0.25)), 72.);
    }

    #[test]
    fn stats_separate_work_from_sleep() {
        // Capped at 100 fps, the game needed 6 ms of each 10 ms.
        let samples = vec![Sample { interval_us: 10_000, slept_us: 4_000 }; 300];
        let s = stats(&samples);
        assert!((s.avg_fps - 100.).abs() < 0.5);
        assert!((s.capacity_fps - 166.7).abs() < 0.5);
        assert_eq!(s.frames, 200);
    }
}
