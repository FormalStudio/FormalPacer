//! Whether this process is a game, decided once from its environment.

#[cfg(windows)]
use crate::library;

/// Launchers and tools that render with Vulkan but are not the game: gamescope inherits the
/// game's Steam variables when it wraps one, and pacing it as well would cap twice.
const NOT_GAMES: &[&str] = &[
    "gamescope",
    "steam",
    "steamwebhelper",
    "formalpacer",
    "fossilize_replay",
    "steam.exe",
    "steamwebhelper.exe",
    "formalpacer.exe",
    "fossilize-replay64.exe",
];

pub struct Game {
    pub name: String,
    pub steam_app_id: u32,
}

pub fn detect() -> Option<Game> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty() && v != "0");
    let forced = std::env::var("FORMALPACER").ok();
    let name = process_name();
    let steam_app_id = ["SteamGameId", "SteamAppId", "STEAM_COMPAT_APP_ID"]
        .iter()
        .find_map(|k| var(k).and_then(|v| v.parse::<u32>().ok()))
        .unwrap_or(0);
    let is_game = match forced.as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => {
            !NOT_GAMES.contains(&name.to_ascii_lowercase().as_str()) && (steam_app_id != 0 || looks_like_a_game(&name))
        }
    };
    (is_game && !protected()).then_some(Game { name, steam_app_id })
}

/// Under Wine every program is an `.exe`, and a Wine prefix only holds games and their launchers.
#[cfg(not(windows))]
fn looks_like_a_game(name: &str) -> bool {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty() && v != "0");
    var("WINEPREFIX").is_some()
        || var("STEAM_COMPAT_DATA_PATH").is_some()
        || name.to_ascii_lowercase().ends_with(".exe")
}

/// On Windows everything is an `.exe`, so the install location decides.
#[cfg(windows)]
fn looks_like_a_game(_name: &str) -> bool {
    std::env::current_exe().is_ok_and(|exe| library::is_game(&exe))
}

/// Games under a kernel anti-cheat are never paced from inside: a module in their process is
/// what those systems look for. Proton runs them without the kernel part, so this is Windows only.
#[cfg(windows)]
fn protected() -> bool {
    std::env::current_exe().is_ok_and(|exe| library::anticheat(&exe).is_some())
}

#[cfg(not(windows))]
fn protected() -> bool {
    false
}

/// For a hook that is only ever loaded on purpose (the OpenGL hook goes in through
/// `formalpacer run`), every process is a game except the known tools and `FORMALPACER=0`.
pub fn detect_injected() -> Option<Game> {
    let name = process_name();
    let off = std::env::var("FORMALPACER").is_ok_and(|v| v == "0")
        || NOT_GAMES.contains(&name.to_ascii_lowercase().as_str())
        || protected();
    let steam_app_id = ["SteamGameId", "SteamAppId", "STEAM_COMPAT_APP_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok()))
        .unwrap_or(0);
    (!off).then_some(Game { name, steam_app_id })
}

#[cfg(windows)]
fn process_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// The program's own name: under Wine `argv[0]` is the Windows path of the `.exe`.
#[cfg(not(windows))]
fn process_name() -> String {
    let argv0 = std::fs::read("/proc/self/cmdline")
        .ok()
        .and_then(|c| c.split(|&b| b == 0).next().map(|s| String::from_utf8_lossy(s).into_owned()))
        .unwrap_or_default();
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or_default().to_owned();
    if base.is_empty() {
        std::fs::read_to_string("/proc/self/comm").map(|s| s.trim().to_owned()).unwrap_or_default()
    } else {
        base
    }
}
