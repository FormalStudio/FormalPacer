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
/// Under a blind cap (see [`Pacer::update`]) the rate a game reaches is all there is, and one
/// hitch takes it a few percent under the cap for as long as the hitch is in the stats window:
/// a game falls short only when it stays under the cap longer than that.
const BLIND_SHORT_S: f64 = WINDOW_S + 0.5;
/// A game this far under the cap it had before a raise is in a loading screen or a replay, which
/// says nothing about the raise.
const BLIND_SCENE: f64 = 0.9;
/// How long the stats take to hold only frames from after a change (the stats window, and the
/// second PresentMon takes to report a frame): after a new blind cap, or a game coming back to
/// the front.
pub const SETTLE_S: f64 = WINDOW_S + 1.0;
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
/// A game whose slowest 1% of frames and its work both stay within this of its average holds
/// that rate by itself.
const STEADY: f64 = 0.95;

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
/// frame blocked in present, which reads as work; it can do at least the refresh rate. The same
/// goes for a game holding a steady rate of its own (its own limiter, or the driver's) with
/// nothing left to spare: the wait is in its work, so it can do at least that rate, and keeping
/// headroom under it would cap it below a rate it already holds evenly.
pub fn capacity(stats: &Stats, refresh_hz: f64) -> f64 {
    let vsync_locked =
        (stats.avg_fps - refresh_hz).abs() <= refresh_hz * 0.01 && stats.capacity_fps >= refresh_hz * 0.95;
    let self_limited = stats.low_fps >= stats.avg_fps * STEADY && stats.capacity_fps <= stats.avg_fps / STEADY;
    if vsync_locked {
        stats.capacity_fps.max(refresh_hz / HEADROOM)
    } else if self_limited {
        stats.capacity_fps.max(stats.avg_fps.round() / HEADROOM)
    } else {
        stats.capacity_fps
    }
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
            // The small margin keeps a budget that is a multiple of 5 in float error on its step.
            let steady = (((budget + 1e-6) / 5.).floor() * 5.).max(floor);
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
    /// When the game first fell short of the cap, while it still is.
    short_since: Option<f64>,
    /// When the cap last went up, until that raise has held for `FAILED_WITHIN_S`.
    raised_at: Option<f64>,
    /// The cap before that raise.
    raised_from: f64,
    /// When the cap last changed.
    changed_at: f64,
    /// A raise failed: the game's work under a cap is not to be trusted.
    stepping: bool,
    /// No raise before this time, after a failed one.
    blocked_until: f64,
    backoff_s: f64,
}

impl Pacer {
    /// Feeds the newest stats at time `now_s`; returns the cap to hold. `blind` when the cap is
    /// held by a limiter that waits inside the game's frame where the measuring cannot tell the
    /// wait from work (RTSS under PresentMon): under such a cap a game's work looks exactly as
    /// long as the cap's interval, so only the rate it reaches counts. One that holds the cap
    /// may have more to give, which is found out by raising it; one that falls short has shown
    /// what it can do, and is capped under it with the usual headroom.
    pub fn update(&mut self, stats: &Stats, display: &Display, now_s: f64, blind: bool) -> Option<Cap> {
        // Too little data, or a loading screen: hold what we have.
        if stats.span_s < 1.0 || stats.avg_fps < 10. {
            return self.cap;
        }
        let before = self.cap;
        let (short, wanted) = match self.cap {
            // Under a blind cap the rate reached is all there is, and until the window holds only
            // frames from after the last change it is the old cap's.
            Some(_) if blind && now_s - self.changed_at < SETTLE_S => return self.cap,
            Some(current) if blind => {
                let short = stats.avg_fps < current.fps * LOWER_AT;
                let wanted = if !short {
                    // A quarter up at a time, until a raise fails and the raises go a step at a time.
                    plan_budget(current.fps * 1.25, display)
                        .filter(|c| c.fps > current.fps + 0.01)
                        .or_else(|| next_step(current, display))
                        .or(Some(current))
                } else {
                    plan(stats.avg_fps, display)
                };
                (short, wanted)
            }
            current => {
                let capacity = capacity(stats, display.refresh_hz);
                (current.is_some_and(|c| capacity * LOWER_AT < c.fps), plan(capacity, display))
            }
        };
        let Some(current) = self.cap else {
            self.cap = wanted;
            self.changed_at = now_s;
            return self.cap;
        };
        let wanted_fps = wanted.map_or(0., |c| c.fps);
        if self.raised_at.is_some_and(|t| now_s - t > FAILED_WITHIN_S) {
            self.raised_at = None;
            self.backoff_s = BACKOFF_START_S;
        }
        if !self.fits(current, display) {
            self.adopt(wanted);
        } else if short {
            self.short_ticks += 1;
            let since = *self.short_since.get_or_insert(now_s);
            if self.short_ticks >= 2 && (!blind || now_s - since >= BLIND_SHORT_S) {
                let failed =
                    self.raised_at.take().is_some() && (!blind || stats.avg_fps >= self.raised_from * BLIND_SCENE);
                if failed {
                    self.stepping = true;
                    let backoff = if self.backoff_s > 0. { self.backoff_s } else { BACKOFF_START_S };
                    self.blocked_until = now_s + backoff;
                    self.backoff_s = (backoff * 2.).min(BACKOFF_MAX_S);
                }
                self.adopt(wanted);
            }
        } else if wanted_fps > current.fps + 0.01 && now_s >= self.blocked_until {
            self.short_ticks = 0;
            self.short_since = None;
            let since = *self.higher_since.get_or_insert(now_s);
            if now_s - since >= RAISE_AFTER_S {
                let raise = if self.stepping { next_step(current, display) } else { wanted };
                self.adopt(raise);
                self.raised_at = Some(now_s);
                self.raised_from = current.fps;
            }
        } else {
            self.short_ticks = 0;
            self.short_since = None;
            self.higher_since = None;
        }
        if self.cap != before {
            self.changed_at = now_s;
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
        self.short_since = None;
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

    /// An uncapped game at `capacity`, with the spread of frame times an uncapped game has.
    fn steady(capacity: f64) -> Stats {
        Stats { avg_fps: capacity, low_fps: capacity * 0.9, capacity_fps: capacity, frames: 200, span_s: 2. }
    }

    #[test]
    fn drops_fast_and_rises_slow() {
        let p = panel(144., Vrr::Unsupported);
        let mut pacer = Pacer::default();
        assert_eq!(fps(pacer.update(&steady(160.), &p, 0., false)), 144.);
        // One short tick is a blip, two are a trend.
        assert_eq!(fps(pacer.update(&steady(100.), &p, 0.25, false)), 144.);
        assert_eq!(fps(pacer.update(&steady(100.), &p, 0.5, false)), 72.);
        // A brief recovery is not enough to go back up.
        assert_eq!(fps(pacer.update(&steady(170.), &p, 1., false)), 72.);
        assert_eq!(fps(pacer.update(&steady(170.), &p, 5., false)), 72.);
        assert_eq!(fps(pacer.update(&steady(170.), &p, 6.1, false)), 144.);
        // Small wobble under the headroom does not move the cap.
        assert_eq!(fps(pacer.update(&steady(149.), &p, 7., false)), 144.);
        assert_eq!(fps(pacer.update(&steady(149.), &p, 7.5, false)), 144.);
    }

    #[test]
    fn a_cap_that_hides_the_work_is_probed_one_step_up() {
        let mut p = panel(144., Vrr::Active);
        p.vrr_range = Some((30., 144.));
        let mut pacer = Pacer::default();
        assert_eq!(fps(pacer.update(&steady(100.), &p, 0., true)), 90.);
        // Under the cap the limiter's wait counts as work: the game looks exactly as fast as the
        // cap, frame times as uneven as they come, and that never takes the cap down.
        let held = Stats { avg_fps: 89.6, low_fps: 70., capacity_fps: 89.6, frames: 200, span_s: 2. };
        for t in [0.25, 0.5, 1., 3.1] {
            assert_eq!(fps(pacer.update(&held, &p, t, true)), 90., "at {t}");
        }
        assert_eq!(fps(pacer.update(&held, &p, 8.2, true)), 110.);
        // For a while the window still holds frames from under 90.
        for t in [8.45, 8.7, 10.] {
            assert_eq!(fps(pacer.update(&held, &p, t, true)), 110., "at {t}");
        }
        // Read as work, the same numbers would take the cap down a step every few ticks.
        let mut seeing = Pacer::default();
        seeing.update(&steady(100.), &p, 0., false);
        seeing.update(&held, &p, 1., false);
        assert!(fps(seeing.update(&held, &p, 1.25, false)) < 90.);
        // A hitch takes it a few percent under the cap: that is not falling short.
        let hitch = Stats { avg_fps: 101., low_fps: 40., capacity_fps: 101., frames: 200, span_s: 2. };
        for t in [11.3, 11.55, 11.8] {
            assert_eq!(fps(pacer.update(&hitch, &p, t, true)), 110., "at {t}");
        }
        // 110 it stays under: down under what it reaches, no new try for 30 s, then a step at a
        // time.
        let short = Stats { avg_fps: 104., low_fps: 90., capacity_fps: 104., frames: 200, span_s: 2. };
        assert_eq!(fps(pacer.update(&short, &p, 12.3, true)), 110.);
        assert_eq!(fps(pacer.update(&short, &p, 13.9, true)), 95.);
        let held = Stats { avg_fps: 94.6, low_fps: 80., capacity_fps: 94.6, frames: 200, span_s: 2. };
        assert_eq!(fps(pacer.update(&held, &p, 20., true)), 95.);
        assert_eq!(fps(pacer.update(&held, &p, 44., true)), 95.);
        assert_eq!(fps(pacer.update(&held, &p, 49.1, true)), 100.);
    }

    #[test]
    fn a_loading_screen_under_a_blind_cap_is_not_a_failed_raise() {
        let mut p = panel(144., Vrr::Active);
        p.vrr_range = Some((30., 144.));
        let mut pacer = Pacer::default();
        assert_eq!(fps(pacer.update(&steady(100.), &p, 0., true)), 90.);
        let at =
            |fps: f64| Stats { avg_fps: fps - 0.4, low_fps: fps * 0.8, capacity_fps: fps, frames: 200, span_s: 2. };
        pacer.update(&at(90.), &p, 3.1, true);
        assert_eq!(fps(pacer.update(&at(90.), &p, 8.2, true)), 110.);
        // Right after the raise, a loading screen at 40.
        pacer.update(&at(40.), &p, 11.3, true);
        assert_eq!(fps(pacer.update(&at(40.), &p, 13.9, true)), 35.);
        // Play again, held at 35: the climb goes on at once, a quarter at a time.
        pacer.update(&at(35.), &p, 16.95, true);
        assert_eq!(fps(pacer.update(&at(35.), &p, 22., true)), 36.);
        pacer.update(&at(36.), &p, 25.05, true);
        assert_eq!(fps(pacer.update(&at(36.), &p, 30.1, true)), 45.);
    }

    #[test]
    fn a_game_whose_work_lies_under_a_cap_climbs_one_step_at_a_time() {
        let mut p = panel(144., Vrr::Active);
        p.vrr_range = Some((30., 144.));
        let mut pacer = Pacer::default();
        // Uncapped, the game really runs at 40.
        assert_eq!(fps(pacer.update(&steady(40.), &p, 0., false)), 35.);
        // Capped, its other threads work through the layer's sleep: it looks 50 times faster.
        let looks_fast = steady(2000.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 1., false)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 6.1, false)), 138.);
        // It cannot hold that: back down, and no new try for 30 s however fast it looks.
        assert_eq!(fps(pacer.update(&steady(40.), &p, 6.5, false)), 138.);
        assert_eq!(fps(pacer.update(&steady(40.), &p, 6.75, false)), 35.);
        for t in [7., 20., 35.7] {
            assert_eq!(fps(pacer.update(&looks_fast, &p, t, false)), 35., "at {t}");
        }
        // Then, after the usual five seconds, one step up rather than back to 138.
        assert_eq!(fps(pacer.update(&looks_fast, &p, 37., false)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 42.1, false)), 36.);
        // 36 holds; the next step, 45, does not: down again, and the next try waits 60 s.
        assert_eq!(fps(pacer.update(&looks_fast, &p, 43., false)), 36.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 48.1, false)), 45.);
        assert_eq!(fps(pacer.update(&steady(40.), &p, 48.3, false)), 45.);
        assert_eq!(fps(pacer.update(&steady(40.), &p, 48.55, false)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 100., false)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 109., false)), 35.);
        assert_eq!(fps(pacer.update(&looks_fast, &p, 114.1, false)), 36.);
    }

    #[test]
    fn vsync_locked_game_keeps_the_full_rate() {
        let p = panel(144., Vrr::Unsupported);
        let mut pacer = Pacer::default();
        // Blocked in present for vsync: work reads as 140 fps while the game holds 144.
        let locked = Stats { avg_fps: 143.8, low_fps: 140., capacity_fps: 140., frames: 288, span_s: 2. };
        assert_eq!(fps(pacer.update(&locked, &p, 0., false)), 144.);
        assert_eq!(fps(pacer.update(&locked, &p, 0.25, false)), 144.);
        assert_eq!(fps(pacer.update(&locked, &p, 0.5, false)), 144.);
    }

    #[test]
    fn a_game_holding_its_own_rate_is_not_capped_under_it() {
        let mut p = panel(144., Vrr::Active);
        p.vrr_range = Some((30., 144.));
        // A menu held at 120 by the game's own limiter: every frame's wait is in its work.
        let own = Stats { avg_fps: 119.9, low_fps: 117., capacity_fps: 118.5, frames: 240, span_s: 2. };
        assert_eq!(fps(plan(capacity(&own, 144.), &p)), 120.);
        let own = Stats { avg_fps: 59.94, low_fps: 59., capacity_fps: 59.5, frames: 120, span_s: 2. };
        assert_eq!(fps(plan(capacity(&own, 144.), &p)), 60.);
        let mut pacer = Pacer::default();
        for t in [0., 0.25, 0.5, 10., 20.] {
            assert_eq!(fps(pacer.update(&own, &p, t, false)), 60., "at {t}");
        }
        // Uneven frames are not a limiter: the headroom stays.
        let rough = Stats { avg_fps: 120., low_fps: 80., capacity_fps: 110., frames: 240, span_s: 2. };
        assert_eq!(fps(plan(capacity(&rough, 144.), &p)), 100.);
        // Under FormalPacer's own cap the work shows the room to spare, which the plan uses as is.
        let capped = Stats { avg_fps: 90., low_fps: 89., capacity_fps: 140., frames: 180, span_s: 2. };
        assert_eq!(capacity(&capped, 144.), 140.);
    }

    #[test]
    fn follows_vrr_switching_off() {
        let mut pacer = Pacer::default();
        assert_eq!(fps(pacer.update(&steady(100.), &panel(144., Vrr::Active), 0., false)), 90.);
        assert_eq!(fps(pacer.update(&steady(100.), &panel(144., Vrr::Off), 0.25, false)), 72.);
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
