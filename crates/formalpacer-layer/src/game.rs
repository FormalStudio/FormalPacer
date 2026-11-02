//! Whether this process is a game, decided once from its environment.

/// Launchers and tools that render with Vulkan but are not the game: gamescope inherits the
/// game's Steam variables when it wraps one, and pacing it as well would cap twice.
const NOT_GAMES: &[&str] = &["gamescope", "steam", "steamwebhelper", "formalpacer", "fossilize_replay"];

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
            !NOT_GAMES.contains(&name.as_str())
                && (steam_app_id != 0
                    || var("WINEPREFIX").is_some()
                    || var("STEAM_COMPAT_DATA_PATH").is_some()
                    || name.to_ascii_lowercase().ends_with(".exe"))
        }
    };
    is_game.then_some(Game { name, steam_app_id })
}

/// The program's own name: under Wine `argv[0]` is the Windows path of the `.exe`.
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
