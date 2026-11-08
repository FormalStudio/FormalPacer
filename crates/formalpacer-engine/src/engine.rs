//! The engine thread: attaches to every game the layer reports (and on Windows every game
//! PresentMon sees presenting), keeps each one's cap, and sends the app a snapshot whenever what it
//! would show changes. It runs whether or not a window is open, so pacing goes on from the tray.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use formalpacer_shm::{Mapping, Overlay, Sample, VrrState, now_ns, prefix, shm_dir};

use crate::config::{self, Config, Mode};
use crate::display::{self, Display, Vrr};
use crate::layer::{self, LayerState};
use crate::pacing::{self, Cap, Pacer, Stats};
#[cfg(windows)]
use crate::{driver, floating, presentmon, rtss};
use crate::{placement, process, steam};

const TICK: Duration = Duration::from_millis(250);
const IDLE_TICK: Duration = Duration::from_secs(1);
const PROBE_EVERY: Duration = Duration::from_secs(1);
/// A game that has not presented for this long is in the background or gone.
const LIVE_NS: u64 = 1_500_000_000;
const SWEEP_NS: u64 = 10_000_000_000;
/// Enough for the planner's two seconds at any frame rate: a read gets at most a ring's worth
/// every tick, about 2000 samples a second.
const KEEP_SAMPLES: usize = 8192;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetMode(Mode),
    SetDisplay(Option<String>),
    SetOverlay(bool),
    InstallLayer,
    /// Windows: asks for an administrator to add this user to Performance Log Users, which
    /// PresentMon needs.
    JoinPerformanceLogUsers,
    /// Windows: asks for an administrator to let this user write RTSS's profiles.
    AllowRtss,
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
    pub limiter: Limiter,
}

/// What holds the cap for a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Limiter {
    /// FormalPacer's own code inside the game (the Vulkan layer, or the hook on Windows), from
    /// the next frame.
    InGame,
    /// The NVIDIA driver, from the game's next launch: it reads its profile when a game starts.
    /// `running` is what it holds for the run now going, if anything.
    /// `next` is what the game's next launch gets, when that differs.
    Driver { running: Option<u32>, next: Option<u32>, protected: bool },
    /// RTSS, which the game lets in, from the next frame; `hooked` once RTSS is in the game.
    /// While RTSS runs the driver holds no caps: one would hold the next launch under RTSS's.
    Rtss { protected: bool, hooked: bool },
    /// Nothing can: the game is measured only. `protected` when a kernel anti-cheat is why the
    /// layer stays out and there is no NVIDIA driver to ask instead.
    None { protected: bool },
}

/// Measuring games from outside, through PresentMon on Windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outside {
    Starting,
    Running,
    /// Not an administrator and not in the Performance Log Users group.
    Denied,
    /// In Performance Log Users, from the next sign-in: Windows reads groups at sign-in.
    SignIn,
    /// `PresentMon.exe` is not next to FormalPacer.
    Missing,
    Failed(String),
}

/// RivaTuner Statistics Server, which can change a measured game's cap while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rtss {
    Missing,
    /// Installed, not running.
    Stopped,
    /// Running, but only administrators may write its profiles until FormalPacer is let in.
    Locked,
    Ready,
}

/// How the game's display was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The user picked it.
    Chosen,
    /// The compositor or X server says the game's window is there.
    Window,
    /// The window's display is unknown; the game renders at exactly this display's mode, so it
    /// is fullscreen there.
    Size,
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
    /// How games without the layer are measured; `None` where only the layer measures (Linux).
    pub outside: Option<Outside>,
    /// `None` where there is no RTSS (Linux).
    pub rtss: Option<Rtss>,
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

/// Where a session's frames come from and where its cap goes.
enum Feed {
    /// A page the layer in the game writes and reads.
    Page(Mapping),
    /// PresentMon measures it; RTSS holds its cap when it is in the game (`live`), the driver
    /// otherwise. Only a game that has been in the foreground gets one, so a launcher or tool
    /// presenting behind it never does.
    #[cfg(windows)]
    Measured { pid: u32, driver: driver::Cap, rtss: rtss::Limit, live: bool, foreground: bool },
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Page(PathBuf),
    #[cfg(windows)]
    Process(u32),
}

struct Session {
    feed: Feed,
    name: String,
    read_to: u64,
    samples: VecDeque<Sample>,
    pacer: Pacer,
    stats: Stats,
    display: String,
    placement: Placement,
    output: Option<String>,
    placed: Option<Instant>,
    toggles: u32,
    /// When the game last came to the front, while it stays there.
    front_since: Option<f64>,
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
    sessions: HashMap<Key, Session>,
    rejected: HashSet<Key>,
    #[cfg(windows)]
    presentmon: presentmon::Feed,
    /// Executables of processes PresentMon has seen, by pid.
    #[cfg(windows)]
    exes: HashMap<u32, Option<PathBuf>>,
    /// Games the hook was loaded into, and when; the hook's page is waited for before they are
    /// measured from outside instead.
    #[cfg(windows)]
    injected: HashMap<u32, Instant>,
    /// Whether the user is in Performance Log Users, checked while PresentMon is denied.
    #[cfg(windows)]
    joined: bool,
    #[cfg(windows)]
    floating: floating::Floating,
    #[cfg(windows)]
    rtss: Rtss,
    #[cfg(windows)]
    osd: rtss::Osd,
    notice: Option<String>,
    last: Option<Snapshot>,
}

impl Feed {
    fn pid(&self) -> u32 {
        match self {
            Self::Page(page) => page.pid(),
            #[cfg(windows)]
            Self::Measured { pid, .. } => *pid,
        }
    }

    fn toggles(&self) -> u32 {
        match self {
            Self::Page(page) => page.toggles(),
            #[cfg(windows)]
            Self::Measured { .. } => 0,
        }
    }
}

impl Engine {
    fn new() -> Self {
        #[allow(unused_mut)]
        let mut config = config::load();
        // Limits left behind by a FormalPacer that did not exit cleanly; a game still running gets
        // its cap again on the next tick.
        #[cfg(windows)]
        if rtss::state() == Rtss::Ready {
            let _ = rtss::restore_all(&mut config);
        }
        Self {
            config,
            layer: LayerState::Missing,
            displays: Vec::new(),
            probed: None,
            sessions: HashMap::new(),
            rejected: HashSet::new(),
            #[cfg(windows)]
            presentmon: presentmon::Feed::start(),
            #[cfg(windows)]
            exes: HashMap::new(),
            #[cfg(windows)]
            injected: HashMap::new(),
            #[cfg(windows)]
            joined: false,
            #[cfg(windows)]
            floating: floating::Floating::start(),
            #[cfg(windows)]
            rtss: Rtss::Missing,
            #[cfg(windows)]
            osd: rtss::Osd::new(),
            notice: None,
            last: None,
        }
    }

    fn outside(&self) -> Option<Outside> {
        #[cfg(windows)]
        return Some(match self.presentmon.state() {
            Outside::Denied if self.joined => Outside::SignIn,
            state => state,
        });
        #[cfg(not(windows))]
        None
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
            Command::SetMode(mode) => {
                self.config.mode = mode;
                #[cfg(windows)]
                if mode == Mode::Off {
                    let originals = self.config.driver_caps.clone();
                    if let Err(e) = driver::restore_all(&mut self.config) {
                        self.notice = Some(format!("Could not clear the driver's frame rate limits: {e}"));
                    }
                    if let Err(e) = rtss::restore_all(&mut self.config) {
                        self.notice = Some(format!("Could not clear RTSS's frame rate limits: {e}"));
                    }
                    for session in self.sessions.values_mut() {
                        if let Feed::Measured { driver, rtss, .. } = &mut session.feed {
                            driver.restored(&originals);
                            rtss.released();
                        }
                    }
                }
            }
            Command::SetDisplay(display) => self.config.display = display,
            Command::SetOverlay(show) => self.config.overlay = show,
            Command::InstallLayer => {
                self.notice = Some(match layer::install() {
                    Ok(path) => format!("Layer registered in {}. Restart running games to pick it up.", path.display()),
                    Err(e) => format!("Could not register the layer: {e}"),
                });
                self.probed = None;
                return;
            }
            Command::JoinPerformanceLogUsers => {
                #[cfg(windows)]
                {
                    self.notice = Some(match crate::elevate::join_performance_log_users() {
                        Ok(()) => "Added to Performance Log Users. Sign out and back in for it to take effect.".into(),
                        Err(e) => format!("Could not join Performance Log Users: {e}"),
                    });
                }
                return;
            }
            Command::AllowRtss => {
                #[cfg(windows)]
                {
                    let allowed = rtss::profiles()
                        .ok_or_else(|| std::io::Error::other("RTSS is not installed"))
                        .and_then(|p| crate::elevate::allow_rtss_profiles(&p));
                    if let Err(e) = allowed {
                        self.notice = Some(format!("Could not let FormalPacer set RTSS's limit: {e}"));
                    }
                    self.rtss = rtss::state();
                }
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
            #[cfg(windows)]
            if self.presentmon.state() == Outside::Denied {
                self.joined = crate::elevate::in_performance_log_users();
            }
            #[cfg(windows)]
            {
                self.rtss = rtss::state();
            }
            self.probed = Some(Instant::now());
        }
        self.scan();
        let now = now_ns();
        let now_s = now as f64 / 1e9;
        let presses = self.sessions.values_mut().fold(0u32, |presses, s| {
            let toggles = s.feed.toggles();
            presses.wrapping_add(toggles.wrapping_sub(std::mem::replace(&mut s.toggles, toggles)))
        });
        if presses % 2 == 1 {
            self.apply(Command::SetOverlay(!self.config.overlay));
        }
        let outside = self.outside();
        #[cfg(windows)]
        let foreground = crate::window::foreground_pid();
        #[cfg(not(windows))]
        let foreground = 0;
        #[cfg(windows)]
        let rtss_ready = self.rtss == Rtss::Ready;
        #[cfg(windows)]
        let hooked = if rtss_ready { rtss::hooked().unwrap_or_default() } else { HashSet::new() };
        let mut active: Option<(&Session, u64)> = None;
        for session in self.sessions.values_mut() {
            let pid = session.feed.pid();
            let (fresh, end, heartbeat, x11_window, swap_extent) = match &session.feed {
                Feed::Page(page) => {
                    let (fresh, end) = page.samples_since(session.read_to);
                    (fresh, end, page.heartbeat_ns(), page.x11_window(), page.swap_extent())
                }
                #[cfg(windows)]
                Feed::Measured { pid, .. } => {
                    let (fresh, end) = self.presentmon.samples_since(*pid, session.read_to);
                    (fresh, end, self.presentmon.heartbeat_ns(*pid), 0, crate::window::game_extent(*pid))
                }
            };
            session.read_to = end;
            session.samples.extend(fresh);
            let excess = session.samples.len().saturating_sub(KEEP_SAMPLES);
            session.samples.drain(..excess);
            session.stats = pacing::stats(session.samples.make_contiguous());

            if session.placed.is_none_or(|t| t.elapsed() >= PROBE_EVERY) {
                session.output = placement::game_output(pid, x11_window);
                session.placed = Some(Instant::now());
            }
            let target =
                pick_display(&self.displays, self.config.display.as_deref(), session.output.as_deref(), swap_extent);
            if let Some((_, how)) = target {
                session.placement = how;
            }
            // A game behind another window often slows itself down, and what it runs at then says
            // nothing about what it can do in front: its cap holds until it is back, and until
            // the stats hold only frames from after that.
            #[cfg(windows)]
            let in_front = pid == foreground;
            #[cfg(not(windows))]
            let in_front = true;
            if !in_front {
                session.front_since = None;
            }
            let settled = in_front && now_s - *session.front_since.get_or_insert(now_s) >= pacing::SETTLE_S;
            #[cfg(windows)]
            let blind = matches!(session.feed, Feed::Measured { .. }) && hooked.contains(&pid);
            #[cfg(not(windows))]
            let blind = false;
            let cap = match (self.config.mode, target.map(|(d, _)| d)) {
                (Mode::Auto, Some(display)) => {
                    if session.display != display.connector {
                        session.pacer = Pacer::default();
                        session.display = display.connector.clone();
                    }
                    if settled {
                        session.pacer.update(&session.stats, display, now_s, blind)
                    } else {
                        session.pacer.cap
                    }
                }
                (_, display) => {
                    session.pacer = Pacer::default();
                    session.display = display.map(|d| d.connector.clone()).unwrap_or_default();
                    None
                }
            };
            match &mut session.feed {
                Feed::Page(page) => {
                    page.set_target(cap.map_or(0, |c| c.interval_ns()), now);
                    page.set_overlay(overlay_for(target.map(|(d, _)| d), self.config.overlay));
                }
                #[cfg(windows)]
                Feed::Measured { driver, rtss, live, foreground: seen, pid } => {
                    *seen |= *pid == foreground;
                    *live = hooked.contains(pid);
                    if self.config.mode == Mode::Auto && *seen {
                        let fps = cap.map(|c| c.fps);
                        let set = if *live {
                            rtss.want(fps, &mut self.config).and_then(|()| driver.release(&mut self.config))
                        } else if rtss_ready {
                            driver.release(&mut self.config)
                        } else {
                            driver.want(fps, &mut self.config)
                        };
                        if let Err(e) = set {
                            let by = if *live { "RTSS's" } else { "the driver's" };
                            self.notice = Some(format!("Could not set {by} frame rate limit: {e}"));
                        }
                    }
                }
            }

            // The game in the foreground is the one shown; otherwise the one that presented last.
            let rank = if pid == foreground { u64::MAX } else { heartbeat };
            if now.saturating_sub(heartbeat) < LIVE_NS && active.is_none_or(|(_, b)| rank > b) {
                active = Some((session, rank));
            }
        }
        #[cfg(windows)]
        {
            let overlay = active.and_then(|(s, _)| {
                let Feed::Measured { pid, driver, rtss, live, .. } = &s.feed else { return None };
                if !self.config.overlay || *pid != foreground {
                    return None;
                }
                let display = self.displays.iter().find(|d| d.connector == s.display);
                // Under both limiters the lower one holds.
                let cap = if *live {
                    [rtss.applied(), driver.running()].into_iter().flatten().min()
                } else {
                    driver.running()
                };
                let shown = overlay_for(display, true);
                let text = formalpacer_hook::overlay::Text {
                    fps: s.stats.avg_fps.round() as u32,
                    cap: cap.unwrap_or(0),
                    next: if *live { 0 } else { driver.next().unwrap_or(0) },
                    vrr: shown.vrr,
                    refresh_hz: u32::from(shown.refresh_hz),
                    vrr_min_hz: u32::from(shown.vrr_min_hz),
                    frames_us: s
                        .samples
                        .range(s.samples.len().saturating_sub(formalpacer_hook::overlay::GRAPH_FRAMES)..)
                        .map(|f| f.interval_us)
                        .collect(),
                };
                Some((text, *pid, *live, vrr_state(display) == VrrState::On))
            });
            let inside = overlay.as_ref().filter(|(_, _, live, _)| *live).map(|(text, ..)| {
                let paced = if text.cap > 0 { text.cap } else { text.fps.max(1) };
                (text.lines().join("\n"), 1e6 / paced as f32)
            });
            self.osd.show(inside.as_ref().map(|(lines, interval)| (lines.as_str(), *interval)));
            // A window over the game makes Windows compose the game, which turns G-SYNC off in
            // its default fullscreen-only mode: the window is for a game RTSS is not in, on a
            // display without VRR turned on. Not "running": the window itself stops it running.
            let shown = overlay
                .filter(|(_, _, live, vrr_on)| !live && !vrr_on)
                .and_then(|(text, pid, ..)| Some(floating::Shown { text, monitor: floating::monitor_of(pid)? }));
            self.floating.show(shown);
        }
        Snapshot {
            config: self.config.clone(),
            layer: self.layer.clone(),
            outside,
            #[cfg(windows)]
            rtss: Some(self.rtss),
            #[cfg(not(windows))]
            rtss: None,
            displays: self.displays.clone(),
            game: active.map(|(s, _)| Game {
                name: s.name.clone(),
                steam_app_id: match &s.feed {
                    Feed::Page(page) => page.steam_app_id(),
                    #[cfg(windows)]
                    Feed::Measured { .. } => 0,
                },
                stats: rounded(s.stats),
                cap: s.pacer.cap,
                display: s.display.clone(),
                placement: s.placement,
                limiter: match &s.feed {
                    Feed::Page(_) => Limiter::InGame,
                    #[cfg(windows)]
                    Feed::Measured { driver, live, .. } if *live || self.rtss == Rtss::Ready => {
                        Limiter::Rtss { protected: driver.protected(), hooked: *live }
                    }
                    #[cfg(windows)]
                    Feed::Measured { driver, .. } => driver.limiter(),
                },
            }),
            notice: self.notice.clone(),
        }
    }

    /// Attaches new pages, and removes the files of games that exited without cleaning up.
    fn scan(&mut self) {
        let dir = shm_dir();
        let prefix = prefix();
        let now = now_ns();
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let named = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix));
            let key = Key::Page(path.clone());
            if !named || self.sessions.contains_key(&key) || self.rejected.contains(&key) {
                continue;
            }
            match Mapping::attach(&path) {
                // A program that is not a game, with a hook an older build let in or a layer forced
                // on: it is left uncapped and without the overlay.
                #[cfg(windows)]
                Ok(page)
                    if !process::exe_path(page.pid()).is_some_and(|exe| formalpacer_hook::library::may_hook(&exe)) =>
                {
                    page.set_target(0, now);
                    page.set_overlay(Overlay::default());
                    self.rejected.insert(key);
                }
                Ok(page) => {
                    let id = page.steam_app_id();
                    let name = (id != 0).then(|| steam::app_name(id)).flatten().unwrap_or_else(|| page.name());
                    self.sessions.insert(key, Session::new(Feed::Page(page), name));
                }
                Err(_) => {
                    self.rejected.insert(key);
                }
            }
        }
        #[cfg(windows)]
        self.scan_presenters();
        #[cfg(windows)]
        let presentmon = self.presentmon.clone();
        #[cfg(windows)]
        let mut ended = Vec::new();
        self.sessions.retain(|key, s| {
            let (beat, exists) = match (&s.feed, key) {
                (Feed::Page(page), Key::Page(path)) => (page.heartbeat_ns(), path.exists()),
                #[cfg(windows)]
                (Feed::Measured { pid, .. }, _) => (presentmon.heartbeat_ns(*pid), true),
                #[cfg(windows)]
                _ => (0, false),
            };
            let gone = now.saturating_sub(beat) > SWEEP_NS && !process::alive(s.feed.pid());
            if gone {
                match &s.feed {
                    Feed::Page(page) if exists => page.unlink(),
                    Feed::Page(_) => {}
                    #[cfg(windows)]
                    Feed::Measured { pid, rtss, .. } => {
                        presentmon.forget(*pid);
                        ended.push(rtss.exe().to_owned());
                    }
                }
            }
            exists && !gone
        });
        // A game's RTSS profile is its own again once it exits, unless another copy still runs.
        #[cfg(windows)]
        for exe in ended {
            let running =
                self.sessions.values().any(|s| matches!(&s.feed, Feed::Measured { rtss, .. } if rtss.exe() == exe));
            if !running && let Err(e) = rtss::release(&exe, &mut self.config) {
                self.notice = Some(format!("Could not clear RTSS's frame rate limit: {e}"));
            }
        }
    }

    /// Games PresentMon sees that no page reports: Direct3D and OpenGL games, Vulkan games
    /// without the layer, and every game under a kernel anti-cheat. A game is a process installed
    /// where stores put games, or one that ships as a game whose own window is in the foreground
    /// and fills its monitor (a desktop, a browser or a video player fills it too).
    /// The hook goes into each one without an anti-cheat, which then reports through a page like
    /// the layer does; a game it does not reach in time is measured from outside.
    #[cfg(windows)]
    fn scan_presenters(&mut self) {
        use formalpacer_hook::library;
        let paced: HashSet<u32> = self.sessions.values().map(|s| s.feed.pid()).collect();
        let foreground = crate::window::foreground_pid();
        let mut presenters = self.presentmon.presenters();
        let presenting: HashSet<u32> = presenters.iter().map(|p| p.pid).collect();
        // The hook needs no PresentMon: a foreground game still gets it when PresentMon is denied.
        if foreground != 0 && !presenting.contains(&foreground) {
            presenters.push(presentmon::Presenter { pid: foreground, name: String::new() });
        }
        self.exes.retain(|pid, _| presenters.iter().any(|p| p.pid == *pid));
        self.injected.retain(|pid, _| presenters.iter().any(|p| p.pid == *pid));
        for presenter in presenters {
            let pid = presenter.pid;
            if paced.contains(&pid) || pid == std::process::id() {
                continue;
            }
            let Some(exe) = self.exes.entry(pid).or_insert_with(|| process::exe_path(pid)).clone() else { continue };
            let installed = library::is_game(&exe);
            let fullscreen = pid == foreground
                && !library::not_a_game(&exe)
                && crate::window::fills_monitor(pid)
                && library::has_engine_files(&exe);
            if !installed && !fullscreen {
                continue;
            }
            let protected = library::anticheat(&exe).is_some();
            if !protected {
                match self.injected.get(&pid) {
                    None => {
                        if let Some(hook) = hook_dll() {
                            // A game that refuses the hook is measured after the same wait.
                            let _ = process::inject(pid, &hook);
                        }
                        self.injected.insert(pid, Instant::now());
                        continue;
                    }
                    Some(at) if at.elapsed() < HOOK_WAIT => continue,
                    Some(_) => {}
                }
            }
            if !presenting.contains(&pid) {
                continue;
            }
            let driver = driver::Cap::new(&presenter.name, pid, protected, &self.config);
            let rtss = rtss::Limit::new(&presenter.name);
            let name = steam::app_name_for(&exe).unwrap_or(presenter.name);
            let feed = Feed::Measured { pid, driver, rtss, live: false, foreground: pid == foreground };
            self.sessions.insert(Key::Process(pid), Session::new(feed, name));
        }
    }
}

/// How long a game the hook went into has to report through a page.
#[cfg(windows)]
const HOOK_WAIT: Duration = Duration::from_secs(5);

/// The hook, installed beside FormalPacer.
#[cfg(windows)]
fn hook_dll() -> Option<PathBuf> {
    let dll = std::env::current_exe().ok()?.parent()?.join("formalpacer_inject.dll");
    dll.is_file().then_some(dll)
}

impl Session {
    fn new(feed: Feed, name: String) -> Self {
        let toggles = feed.toggles();
        Self {
            feed,
            name,
            read_to: 0,
            samples: VecDeque::new(),
            pacer: Pacer::default(),
            stats: Stats::default(),
            display: String::new(),
            placement: Placement::Fastest,
            output: None,
            placed: None,
            toggles,
            front_since: None,
        }
    }
}

/// The display a game is paced for: the one the user picked, else the one its window is on, else
/// the one whose mode the game's swapchain matches, else the one with VRR running, else the
/// fastest. Two displays of the same size are told apart by VRR.
pub fn pick_display<'a>(
    displays: &'a [Display],
    chosen: Option<&str>,
    window: Option<&str>,
    swap_extent: (u32, u32),
) -> Option<(&'a Display, Placement)> {
    let named = |name: Option<&str>| name.and_then(|c| displays.iter().find(|d| d.connector == c));
    let sized: Vec<&Display> = displays
        .iter()
        .filter(|d| {
            let (w, h) = (u32::from(d.width), u32::from(d.height));
            swap_extent == (w, h) || swap_extent == (h, w)
        })
        .collect();
    let by_size = match sized.as_slice() {
        [one] => Some(*one),
        many => many.iter().find(|d| d.vrr_active()).copied(),
    };
    named(chosen)
        .map(|d| (d, Placement::Chosen))
        .or_else(|| named(window).map(|d| (d, Placement::Window)))
        .or_else(|| by_size.map(|d| (d, Placement::Size)))
        .or_else(|| displays.iter().find(|d| d.vrr_active()).map(|d| (d, Placement::Vrr)))
        .or_else(|| {
            displays.iter().max_by(|a, b| a.refresh_hz.total_cmp(&b.refresh_hz)).map(|d| (d, Placement::Fastest))
        })
}

/// The overlay's word on `display`: its refresh rate and VRR floor, and whether VRR is on, or
/// where the driver says, whether it runs now. A game composed by Windows (a window over it, an
/// overlay such as Discord's) is on fixed refresh though G-SYNC is turned on.
fn overlay_for(display: Option<&Display>, show: bool) -> Overlay {
    let vrr = match vrr_state(display) {
        #[cfg(windows)]
        VrrState::On
            if display.is_some_and(|d| {
                crate::nvidia::vrr(&format!("\\\\.\\{}", d.connector)).is_some_and(|v| !v.running)
            }) =>
        {
            VrrState::Off
        }
        vrr => vrr,
    };
    Overlay {
        show,
        vrr,
        refresh_hz: display.map_or(0, |d| d.refresh_hz.round() as u16),
        vrr_min_hz: display.and_then(|d| d.vrr_range).map_or(0, |(min, _)| min.ceil() as u16),
    }
}

fn vrr_state(display: Option<&Display>) -> VrrState {
    match display.map(|d| &d.vrr) {
        Some(Vrr::Active) => VrrState::On,
        Some(Vrr::Off) => VrrState::Off,
        Some(Vrr::Unsupported) => VrrState::Unsupported,
        None => VrrState::Unknown,
    }
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
        let pick = |chosen, window| {
            pick_display(&displays, chosen, window, (0, 0)).map(|(d, how)| (d.connector.as_str(), how))
        };
        assert_eq!(pick(None, Some("HDMI-A-1")), Some(("HDMI-A-1", Placement::Window)));
        assert_eq!(pick(Some("DP-2"), Some("HDMI-A-1")), Some(("DP-2", Placement::Chosen)));
        assert_eq!(pick(None, None), Some(("DP-1", Placement::Vrr)));
        // A window on a display that has since gone falls through.
        assert_eq!(pick(None, Some("DP-9")), Some(("DP-1", Placement::Vrr)));
        let fixed = [display("DP-1", 60., Vrr::Off), display("DP-2", 165., Vrr::Off)];
        assert_eq!(
            pick_display(&fixed, None, None, (0, 0)).map(|(d, h)| (d.connector.as_str(), h)),
            Some(("DP-2", Placement::Fastest))
        );
    }

    #[test]
    fn a_fullscreen_game_is_found_by_its_size() {
        let mut tv = display("HDMI-A-1", 60., Vrr::Off);
        (tv.width, tv.height) = (3840, 2160);
        let displays = [display("DP-1", 240., Vrr::Active), tv, display("DP-2", 144., Vrr::Off)];
        let pick = |extent| pick_display(&displays, None, None, extent).map(|(d, how)| (d.connector.as_str(), how));
        assert_eq!(pick((3840, 2160)), Some(("HDMI-A-1", Placement::Size)));
        // DP-1 and DP-2 are both 2560x1440: the one running VRR has the fullscreen game.
        assert_eq!(pick((2560, 1440)), Some(("DP-1", Placement::Size)));
        // A window smaller than any display says nothing.
        assert_eq!(pick((1280, 720)), Some(("DP-1", Placement::Vrr)));
    }
}
