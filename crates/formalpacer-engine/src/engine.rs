//! The engine thread: attaches to every game the layer reports, keeps each one's cap, and sends
//! the app a snapshot whenever what it would show changes. It runs whether or not a window is
//! open, so pacing goes on from the tray.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use formalpacer_shm::{Mapping, Sample, now_ns, prefix, shm_dir};

use crate::config::{self, Config, Mode};
use crate::display::{self, Display};
use crate::layer::{self, LayerState};
use crate::pacing::{self, Cap, Pacer, Stats};
use crate::{placement, steam};

const TICK: Duration = Duration::from_millis(250);
const IDLE_TICK: Duration = Duration::from_secs(1);
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// A game that has not presented for this long is in the background or gone.
const LIVE_NS: u64 = 1_500_000_000;
const SWEEP_NS: u64 = 10_000_000_000;
const KEEP_SAMPLES: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetMode(Mode),
    SetDisplay(Option<String>),
    InstallLayer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    pub name: String,
    pub steam_app_id: u32,
    pub stats: Stats,
    pub cap: Option<Cap>,
    /// Connector of the display the cap was planned for.
    pub display: String,
    pub placement: Placement,
}

/// How the game's display was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The user picked it.
    Chosen,
    /// The compositor or X server says the game's window is there.
    Window,
    /// The window's display is unknown; this one has VRR running, which compositors set for
    /// the fullscreen game.
    Vrr,
    /// Unknown, and no display runs VRR: the fastest one.
    Fastest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub config: Config,
    pub layer: LayerState,
    pub displays: Vec<Display>,
    pub game: Option<Game>,
    /// The outcome of the last layer install, for the app to show once.
    pub notice: Option<String>,
}

impl Snapshot {
    pub fn display(&self, connector: &str) -> Option<&Display> {
        self.displays.iter().find(|d| d.connector == connector)
    }
}

pub struct Handle {
    pub commands: flume::Sender<Command>,
    pub snapshots: flume::Receiver<Snapshot>,
}

struct Session {
    page: Mapping,
    name: String,
    read_to: u64,
    samples: VecDeque<Sample>,
    pacer: Pacer,
    stats: Stats,
    display: String,
    placement: Placement,
    output: Option<String>,
    placed: Option<Instant>,
}

pub fn start() -> std::io::Result<Handle> {
    let (commands, command_rx) = flume::unbounded();
    let (snapshot_tx, snapshots) = flume::unbounded();
    std::thread::Builder::new().name("formalpacer-engine".into()).spawn(move || {
        Engine::new().run(&command_rx, &snapshot_tx);
    })?;
    Ok(Handle { commands, snapshots })
}

struct Engine {
    config: Config,
    layer: LayerState,
    displays: Vec<Display>,
    probed: Option<Instant>,
    sessions: HashMap<PathBuf, Session>,
    rejected: HashSet<PathBuf>,
    notice: Option<String>,
    last: Option<Snapshot>,
}

impl Engine {
    fn new() -> Self {
        Self {
            config: config::load(),
            layer: LayerState::Missing,
            displays: Vec::new(),
            probed: None,
            sessions: HashMap::new(),
            rejected: HashSet::new(),
            notice: None,
            last: None,
        }
    }

    fn run(mut self, commands: &flume::Receiver<Command>, snapshots: &flume::Sender<Snapshot>) {
        loop {
            let snapshot = self.tick();
            if self.last.as_ref() != Some(&snapshot) {
                if snapshots.send(snapshot.clone()).is_err() {
                    return;
                }
                self.notice = None;
                self.last = Some(snapshot);
            }
            let wait = if self.sessions.is_empty() { IDLE_TICK } else { TICK };
            match commands.recv_timeout(wait) {
                Ok(command) => self.apply(command),
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetMode(mode) => self.config.mode = mode,
            Command::SetDisplay(display) => self.config.display = display,
            Command::InstallLayer => {
                self.notice = Some(match layer::install() {
                    Ok(path) => format!("Layer registered in {}. Restart running games to pick it up.", path.display()),
                    Err(e) => format!("Could not register the layer: {e}"),
                });
                self.probed = None;
                return;
            }
        }
        if let Err(e) = config::save(&self.config) {
            self.notice = Some(format!("Could not save settings: {e}"));
        }
    }

    fn tick(&mut self) -> Snapshot {
        if self.probed.is_none_or(|t| t.elapsed() >= PROBE_EVERY) {
            self.displays = display::probe();
            self.layer = layer::state();
            self.probed = Some(Instant::now());
        }
        self.scan();
        let now = now_ns();
        let now_s = now as f64 / 1e9;
        let mut active: Option<(&Session, u64)> = None;
        for session in self.sessions.values_mut() {
            let (fresh, end) = session.page.samples_since(session.read_to);
            session.read_to = end;
            session.samples.extend(fresh);
            let excess = session.samples.len().saturating_sub(KEEP_SAMPLES);
            session.samples.drain(..excess);
            session.stats = pacing::stats(session.samples.make_contiguous());

            if session.placed.is_none_or(|t| t.elapsed() >= PROBE_EVERY) {
                session.output = placement::game_output(session.page.pid(), session.page.x11_window());
                session.placed = Some(Instant::now());
            }
            let target = pick_display(&self.displays, self.config.display.as_deref(), session.output.as_deref());
            if let Some((_, how)) = target {
                session.placement = how;
            }
            let cap = match (self.config.mode, target.map(|(d, _)| d)) {
                (Mode::Auto, Some(display)) => {
                    if session.display != display.connector {
                        session.pacer = Pacer::default();
                        session.display = display.connector.clone();
                    }
                    session.pacer.update(&session.stats, display, now_s)
                }
                (_, display) => {
                    session.pacer = Pacer::default();
                    session.display = display.map(|d| d.connector.clone()).unwrap_or_default();
                    None
                }
            };
            session.page.set_target(cap.map_or(0, |c| c.interval_ns()), now);

            let beat = session.page.heartbeat_ns();
            if now.saturating_sub(beat) < LIVE_NS && active.is_none_or(|(_, b)| beat > b) {
                active = Some((session, beat));
            }
        }
        Snapshot {
            config: self.config.clone(),
            layer: self.layer.clone(),
            displays: self.displays.clone(),
            game: active.map(|(s, _)| Game {
                name: s.name.clone(),
                steam_app_id: s.page.steam_app_id(),
                stats: rounded(s.stats),
                cap: s.pacer.cap,
                display: s.display.clone(),
                placement: s.placement,
            }),
            notice: self.notice.clone(),
        }
    }

    /// Attaches new pages, and removes the files of games that exited without cleaning up.
    fn scan(&mut self) {
        let dir = shm_dir();
        let prefix = prefix();
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let now = now_ns();
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix));
            if !named || self.sessions.contains_key(&path) || self.rejected.contains(&path) {
                continue;
            }
            match Mapping::attach(&path) {
                Ok(page) => {
                    let id = page.steam_app_id();
                    let name = (id != 0).then(|| steam::app_name(id)).flatten().unwrap_or_else(|| page.name());
                    self.sessions.insert(
                        path,
                        Session {
                            page,
                            name,
                            read_to: 0,
                            samples: VecDeque::new(),
                            pacer: Pacer::default(),
                            stats: Stats::default(),
                            display: String::new(),
                            placement: Placement::Fastest,
                            output: None,
                            placed: None,
                        },
                    );
                }
                Err(_) => {
                    self.rejected.insert(path);
                }
            }
        }
        self.sessions.retain(|path, s| {
            let exists = path.exists();
            let gone = now.saturating_sub(s.page.heartbeat_ns()) > SWEEP_NS
                && !PathBuf::from(format!("/proc/{}", s.page.pid())).exists();
            if exists && gone {
                s.page.unlink();
            }
            exists && !gone
        });
    }
}

/// The display a game is paced for: the one the user picked, else the one its window is on, else
/// the one with VRR running, else the fastest.
pub fn pick_display<'a>(
    displays: &'a [Display],
    chosen: Option<&str>,
    window: Option<&str>,
) -> Option<(&'a Display, Placement)> {
    let named = |name: Option<&str>| name.and_then(|c| displays.iter().find(|d| d.connector == c));
    named(chosen)
        .map(|d| (d, Placement::Chosen))
        .or_else(|| named(window).map(|d| (d, Placement::Window)))
        .or_else(|| displays.iter().find(|d| d.vrr_active()).map(|d| (d, Placement::Vrr)))
        .or_else(|| {
            displays.iter().max_by(|a, b| a.refresh_hz.total_cmp(&b.refresh_hz)).map(|d| (d, Placement::Fastest))
        })
}

/// Stats to a tenth of a frame, so the app is not woken for noise.
fn rounded(s: Stats) -> Stats {
    let r = |v: f64| (v * 10.).round() / 10.;
    Stats { avg_fps: r(s.avg_fps), low_fps: r(s.low_fps), capacity_fps: r(s.capacity_fps), frames: 0, span_s: 0. }
}

impl Cap {
    /// "48 fps, a third of 144 Hz".
    pub fn describe(&self, refresh_hz: f64) -> String {
        let fps = format!("{:.0} fps", self.fps);
        let hz = format!("{refresh_hz:.0} Hz");
        match self.kind {
            pacing::CapKind::Fraction(1) => format!("{fps}, the refresh rate of {hz}"),
            pacing::CapKind::Fraction(n) => {
                let part = match n {
                    2 => "half".to_owned(),
                    3 => "a third".to_owned(),
                    4 => "a quarter".to_owned(),
                    n => format!("1/{n}"),
                };
                format!("{fps}, {part} of {hz}")
            }
            pacing::CapKind::VrrCeiling => format!("{fps}, just under {hz} to stay inside VRR"),
            pacing::CapKind::VrrSteady => format!("{fps}, steady inside the VRR window"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::Vrr;

    fn display(connector: &str, refresh_hz: f64, vrr: Vrr) -> Display {
        Display {
            connector: connector.into(),
            model: None,
            width: 2560,
            height: 1440,
            refresh_hz,
            max_refresh_hz: refresh_hz,
            vrr,
            vrr_range: None,
        }
    }

    #[test]
    fn the_games_window_beats_a_faster_or_vrr_display() {
        let displays =
            [display("DP-1", 240., Vrr::Active), display("DP-2", 144., Vrr::Off), display("HDMI-A-1", 60., Vrr::Off)];
        let pick = |chosen, window| pick_display(&displays, chosen, window).map(|(d, how)| (d.connector.as_str(), how));
        assert_eq!(pick(None, Some("HDMI-A-1")), Some(("HDMI-A-1", Placement::Window)));
        assert_eq!(pick(Some("DP-2"), Some("HDMI-A-1")), Some(("DP-2", Placement::Chosen)));
        assert_eq!(pick(None, None), Some(("DP-1", Placement::Vrr)));
        // A window on a display that has since gone falls through.
        assert_eq!(pick(None, Some("DP-9")), Some(("DP-1", Placement::Vrr)));
        let fixed = [display("DP-1", 60., Vrr::Off), display("DP-2", 165., Vrr::Off)];
        assert_eq!(
            pick_display(&fixed, None, None).map(|(d, h)| (d.connector.as_str(), h)),
            Some(("DP-2", Placement::Fastest))
        );
    }
}
