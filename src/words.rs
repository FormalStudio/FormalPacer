//! What the window, the tray item and `--headless` say about a snapshot.

use formalpacer_engine::display::{Display, Vrr};
use formalpacer_engine::engine::{Game, Limiter, Outside, Placement, Snapshot};

pub fn vrr(display: &Display) -> String {
    let range = display.vrr_range.map(|(lo, hi)| format!(", {lo:.0}–{hi:.0} Hz")).unwrap_or_default();
    match display.vrr {
        Vrr::Active => format!("On{range}"),
        Vrr::Off => format!("Supported, off{range}"),
        Vrr::Unsupported => "Not supported".into(),
    }
}

pub fn display_name(display: &Display) -> String {
    match &display.model {
        Some(model) => format!("{model} on {}", display.connector),
        None => display.connector.clone(),
    }
}

pub fn placement(placement: Placement) -> &'static str {
    match placement {
        Placement::Chosen => "Chosen in settings",
        Placement::Window => "The game's window is on it",
        Placement::Size => "The game fills it",
        Placement::Vrr => "Has VRR running",
        Placement::Fastest => "The fastest display",
    }
}

/// "48 fps, a third of 144 Hz", or why there is no cap.
pub fn cap(snapshot: &Snapshot, game: &Game) -> String {
    let refresh = snapshot.display(&game.display).map_or(0., |d| d.refresh_hz);
    match (&game.cap, snapshot.config.mode) {
        (_, formalpacer_engine::config::Mode::Off) => "Off: measuring only".into(),
        (Some(cap), _) => cap.describe(refresh),
        // The planner gives up only when even a 20 fps cap would not hold.
        (None, _) if game.stats.capacity_fps > 0. && game.stats.capacity_fps < 22. => {
            "None: the game runs too slowly for a cap to help".into()
        }
        (None, _) => "Measuring".into(),
    }
}

/// Who holds the cap, when it is not FormalPacer inside the game from the next frame.
pub fn limiter(game: &Game) -> Option<String> {
    const ANTICHEAT: &str = "It runs under a kernel anti-cheat, so nothing of FormalPacer's goes inside it.";
    match &game.limiter {
        Limiter::InGame => None,
        Limiter::Driver { running, protected } => {
            let now = match running {
                Some(fps) => format!("This run is held at {fps} fps."),
                None => "This run has no driver limit.".into(),
            };
            let why = if *protected { format!("{ANTICHEAT} ") } else { String::new() };
            Some(format!("{why}The NVIDIA driver holds the cap from the game's next launch. {now}"))
        }
        Limiter::None { protected: true } => {
            Some(format!("{ANTICHEAT} Without an NVIDIA driver to hold a cap, it is measured only."))
        }
        Limiter::None { protected: false } => {
            Some("Measured only: the hook could not get into this game, and without an NVIDIA driver nothing else can cap it.".into())
        }
    }
}

/// Why anti-cheat games are not measured, if they are not.
pub fn outside(snapshot: &Snapshot) -> Option<&'static str> {
    match snapshot.outside.as_ref()? {
        Outside::Denied => Some(
            "Anti-cheat games aren't measured: Windows only lets administrators and Performance Log Users read frame times",
        ),
        Outside::SignIn => Some(
            "Anti-cheat games are measured from your next sign-in: Windows adds you to Performance Log Users then",
        ),
        Outside::Missing => {
            Some("Anti-cheat games aren't measured: PresentMon.exe is missing next to FormalPacer")
        }
        Outside::Failed(_) => {
            Some("Anti-cheat games aren't measured: PresentMon stopped and is restarting")
        }
        Outside::Starting | Outside::Running => None,
    }
}

pub fn fps(v: f64) -> String {
    if v > 0. { format!("{v:.0}") } else { "–".into() }
}

/// The game, its display and the cap, without the frame rates.
pub fn state_line(snapshot: &Snapshot) -> String {
    let Some(game) = &snapshot.game else {
        return match outside(snapshot) {
            Some(why) => format!("no game ({why})"),
            None => "no game".into(),
        };
    };
    let display = snapshot.display(&game.display);
    let held = limiter(game).map(|l| format!(" ({l})")).unwrap_or_default();
    format!(
        "{} on {} ({:.2} Hz, VRR {}, {}): cap {}{held}",
        game.name,
        display.map(display_name).unwrap_or_else(|| "no display".into()),
        display.map_or(0., |d| d.refresh_hz),
        lower_first(&display.map(vrr).unwrap_or_default()),
        lower_first(placement(game.placement)),
        cap(snapshot, game),
    )
}

pub fn rates_line(snapshot: &Snapshot) -> String {
    let Some(game) = &snapshot.game else { return String::new() };
    let s = &game.stats;
    format!(" | avg {} fps, 1% low {} fps, could run {} fps", fps(s.avg_fps), fps(s.low_fps), fps(s.capacity_fps))
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

pub const OVERLAY_HOTKEY: &str = "Frame rate, cap and VRR in the game's top-left corner. Right Shift + F10 toggles it in game, or run formalpacer --toggle-overlay from a key binding.";
