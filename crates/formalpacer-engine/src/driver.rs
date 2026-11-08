//! The cap of a game FormalPacer only measures, held by the NVIDIA driver's frame rate limiter.
//! The driver reads it when the game starts, so the planner's answer for this run is the cap of
//! the next one. A cap is written once the plan has stood for a while, and within a run it only
//! goes up: a game sits in menus, lobbies and replays that run far slower than play, and whatever
//! was written last caps the whole next run. A cap over what the game can do costs nothing; one
//! under it costs frames all run. A plan of no cap (the game is too slow for one to help just
//! then) writes nothing.
//!
//! Whatever the game's profile held before FormalPacer first wrote to it is kept in the config and
//! put back when pacing is switched off. So is the run a cap was written during, with the limit it
//! started with: the profile alone would tell a FormalPacer restarted mid-run the new cap is live.

use std::io;
use std::time::{Duration, Instant};

use crate::config::{Config, DriverRun};
use crate::engine::Limiter;
use crate::nvidia;

const SETTLE: Duration = Duration::from_secs(10);

pub struct Cap {
    exe: String,
    protected: bool,
    /// When the game's process started, which tells its runs apart.
    started: Option<u64>,
    /// What the driver holds for the run now going.
    running: Option<u32>,
    written: Option<u32>,
    /// Whether this run has written a cap yet; the first may lower the last run's.
    wrote: bool,
    wanted: Option<(Option<u32>, Instant)>,
}

impl Cap {
    pub fn new(exe: &str, pid: u32, protected: bool, config: &Config) -> Self {
        let profile = if nvidia::available() { nvidia::cap(exe).ok().flatten().filter(|&v| v > 0) } else { None };
        let started = crate::process::started(pid);
        let run = config.driver_runs.get(exe).filter(|r| started == Some(r.started));
        let running = match run {
            Some(r) => (r.held > 0).then_some(r.held),
            None => profile,
        };
        Self { exe: exe.to_owned(), protected, started, running, written: profile, wrote: run.is_some(), wanted: None }
    }

    pub fn limiter(&self) -> Limiter {
        if nvidia::available() {
            Limiter::Driver { running: self.running, next: self.next(), protected: self.protected }
        } else {
            Limiter::None { protected: self.protected }
        }
    }

    pub fn protected(&self) -> bool {
        self.protected
    }

    /// What the driver holds for the run now going.
    pub fn running(&self) -> Option<u32> {
        self.running
    }

    /// The cap the game's next launch gets, when it is not the one this run has.
    pub fn next(&self) -> Option<u32> {
        self.written.filter(|_| self.written != self.running)
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
        }
        if !self.wrote
            && let Some(started) = self.started
        {
            config.driver_runs.insert(self.exe.clone(), DriverRun { started, held: self.running.unwrap_or(0) });
        }
        crate::config::save(config)?;
        nvidia::set_cap(&self.exe, Some(fps))?;
        self.written = Some(fps);
        self.wrote = true;
        Ok(())
    }

    /// Another limiter holds the cap now: the profile gets the game's own limit back, from its
    /// next launch.
    pub fn release(&mut self, config: &mut Config) -> io::Result<()> {
        let Some(&original) = config.driver_caps.get(&self.exe) else { return Ok(()) };
        let original = (original > 0).then_some(original);
        nvidia::set_cap(&self.exe, original)?;
        config.driver_caps.remove(&self.exe);
        crate::config::save(config)?;
        self.written = original;
        self.wrote = false;
        self.wanted = None;
        Ok(())
    }

    /// After [`restore_all`], given the limits it put back: the profile holds the game's own limit
    /// again, so the next plan is written whatever it is.
    pub fn restored(&mut self, originals: &std::collections::BTreeMap<String, u32>) {
        if let Some(&original) = originals.get(&self.exe) {
            self.written = (original > 0).then_some(original);
        }
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
