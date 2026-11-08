//! The cap of a game FormalPacer only measures, held by the NVIDIA driver's frame rate limiter.
//! The driver reads it when the game starts, so the planner's answer for this run is the cap of
//! the next one. A cap is written once the plan has stood for a while, and within a run it only
//! goes up: a game sits in menus, lobbies and replays that run far slower than play, and whatever
//! was written last caps the whole next run. A cap over what the game can do costs nothing; one
//! under it costs frames all run. A plan of no cap (the game is too slow for one to help just
//! then) writes nothing.
//!
//! Whatever the game's profile held before FormalPacer first wrote to it is kept in the config and
//! put back when pacing is switched off.

use std::io;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::engine::Limiter;
use crate::nvidia;

const SETTLE: Duration = Duration::from_secs(10);

pub struct Cap {
    exe: String,
    protected: bool,
    /// What the driver holds for the run now going: the profile's value when the game was found.
    running: Option<u32>,
    written: Option<u32>,
    /// Whether this run has written a cap yet; the first may lower the last run's.
    wrote: bool,
    wanted: Option<(Option<u32>, Instant)>,
}

impl Cap {
    pub fn new(exe: &str, protected: bool) -> Self {
        let running = if nvidia::available() { nvidia::cap(exe).ok().flatten().filter(|&v| v > 0) } else { None };
        Self { exe: exe.to_owned(), protected, running, written: running, wrote: false, wanted: None }
    }

    pub fn limiter(&self) -> Limiter {
        if nvidia::available() {
            Limiter::Driver { running: self.running, protected: self.protected }
        } else {
            Limiter::None { protected: self.protected }
        }
    }

    /// What the driver holds for the run now going.
    pub fn running(&self) -> Option<u32> {
        self.running
    }

    /// The planner's cap for this game now.
    pub fn want(&mut self, fps: Option<f64>, config: &mut Config) -> io::Result<()> {
        if !nvidia::available() {
            return Ok(());
        }
        let fps = fps.map(|f| (f.round() as u32).min(nvidia::MAX_FPS));
        match self.wanted {
            Some((w, _)) if w == fps => {}
            _ => self.wanted = Some((fps, Instant::now())),
        }
        let Some((Some(fps), since)) = self.wanted else { return Ok(()) };
        let lower = self.written.is_some_and(|w| fps <= w);
        if Some(fps) == self.written || since.elapsed() < SETTLE || (self.wrote && lower) {
            return Ok(());
        }
        if !config.driver_caps.contains_key(&self.exe) {
            let original = nvidia::cap(&self.exe)?.unwrap_or(0);
            config.driver_caps.insert(self.exe.clone(), original);
            crate::config::save(config)?;
        }
        nvidia::set_cap(&self.exe, Some(fps))?;
        self.written = Some(fps);
        self.wrote = true;
        Ok(())
    }

    /// After [`restore_all`]: the profile holds the game's own limit again, so the next plan is
    /// written whatever it is.
    pub fn restored(&mut self) {
        self.written = None;
        self.wrote = false;
        self.wanted = None;
    }
}

/// Puts back every game's own limit, and forgets them.
pub fn restore_all(config: &mut Config) -> io::Result<()> {
    if !nvidia::available() {
        return Ok(());
    }
    for (exe, original) in std::mem::take(&mut config.driver_caps) {
        nvidia::set_cap(&exe, (original > 0).then_some(original))?;
    }
    Ok(())
}
