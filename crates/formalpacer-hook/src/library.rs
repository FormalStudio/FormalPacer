//! Where games are installed on Windows, and which of them run under a kernel anti-cheat. Only
//! paths are looked at, so the app can ask the same questions about a game it never touches.

use std::path::Path;

/// Directories the stores install games into, matched anywhere in the path.
const LIBRARIES: &[&str] = &[
    "\\steamapps\\common\\",
    "\\epic games\\",
    "\\gog galaxy\\games\\",
    "\\gog games\\",
    "\\xboxgames\\",
    "\\windowsapps\\",
    "\\riot games\\",
    "\\ea games\\",
    "\\electronic arts\\",
    "\\ubisoft game launcher\\games\\",
    "\\battle.net\\",
    "\\call of duty\\",
];

/// Launchers, store clients and crash reporters installed in the same places, which present
/// frames of their own but are not games.
const NOT_GAMES: &[&str] = &[
    "epicgameslauncher.exe",
    "epicwebhelper.exe",
    "unrealcefsubprocess.exe",
    "crashreportclient.exe",
    "steam.exe",
    "steamwebhelper.exe",
    "galaxyclient.exe",
    "battle.net.exe",
    "agent.exe",
    "riotclientservices.exe",
    "riotclientux.exe",
    "riotclientuxrender.exe",
    "leagueclient.exe",
    "leagueclientux.exe",
    "leagueclientuxrender.exe",
    "eadesktop.exe",
    "eabackgroundservice.exe",
    "origin.exe",
    "upc.exe",
    "ubisoftconnect.exe",
    "uplaywebcore.exe",
    "gamebar.exe",
    "xboxpcapp.exe",
    "formalpacer.exe",
    // Tools sold on Steam, installed beside the games.
    "wallpaper32.exe",
    "wallpaper64.exe",
    "webwallpaper32.exe",
    "framepacer.exe",
    "framepacer32.exe",
    "dsx.exe",
    "lossless scaling.exe",
];

/// What an anti-cheat installs next to the game it protects: a directory in the game's tree, or a
/// launcher beside the executable whose name carries the system's suffix.
const MARKERS: &[(&str, AntiCheat)] = &[
    ("easyanticheat", AntiCheat::EasyAntiCheat),
    ("easyanticheat_eos", AntiCheat::EasyAntiCheat),
    ("start_protected_game.exe", AntiCheat::EasyAntiCheat),
    ("battleye", AntiCheat::BattlEye),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AntiCheat {
    EasyAntiCheat,
    BattlEye,
    /// Riot Vanguard, Call of Duty's Ricochet, EA Javelin and other systems with no marker in the
    /// game's directory, known by the game's own name.
    Kernel,
}

/// Executables protected by an anti-cheat that leaves no marker beside them.
const KNOWN: &[&str] = &[
    "valorant-win64-shipping.exe",
    "valorant.exe",
    "league of legends.exe",
    "cod.exe",
    "cod24-cod.exe",
    "modernwarfare.exe",
    "blackops6.exe",
    "cs2.exe",
    "dota2.exe",
    "deadlock.exe",
    "bf2042.exe",
    "bf6.exe",
    "fc25.exe",
    "fc26.exe",
    "apex_legends.exe",
    "r5apex.exe",
    "r5apex_dx12.exe",
];

/// A game, judged from outside: installed where stores put games, and not one of their launchers.
pub fn is_game(exe: &Path) -> bool {
    in_game_library(exe) && !not_a_game(exe)
}

/// A launcher or tool known to present frames of its own.
pub fn not_a_game(exe: &Path) -> bool {
    let name = exe.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    NOT_GAMES.contains(&name.as_str())
}

pub fn in_game_library(exe: &Path) -> bool {
    let path = exe.to_string_lossy().to_ascii_lowercase().replace('/', "\\");
    LIBRARIES.iter().any(|l| path.contains(l))
}

/// The anti-cheat protecting the game at `exe`, looked for in the executable's directory and the
/// three above it (Unreal games keep the binary in `Game\Binaries\Win64`), stopping at a library
/// root. VAC titles are in the list too: Valve bans for code in the game's process, as AMD's
/// Anti-Lag+ showed in 2023.
pub fn anticheat(exe: &Path) -> Option<AntiCheat> {
    let name = exe.file_name()?.to_string_lossy().to_ascii_lowercase();
    if KNOWN.contains(&name.as_str()) {
        return Some(AntiCheat::Kernel);
    }
    let stem = name.trim_end_matches(".exe");
    if stem.ends_with("_eac") || stem.ends_with("_eac_eos") {
        return Some(AntiCheat::EasyAntiCheat);
    }
    if stem.ends_with("_be") {
        return Some(AntiCheat::BattlEye);
    }
    let mut dir = exe.parent();
    for _ in 0..4 {
        let Some(d) = dir else { break };
        if let Some(found) = marker_in(d) {
            return Some(found);
        }
        if is_library_root(d) {
            break;
        }
        dir = d.parent();
    }
    None
}

fn marker_in(dir: &Path) -> Option<AntiCheat> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let stem = name.trim_end_matches(".exe");
        if let Some((_, kind)) = MARKERS.iter().find(|(m, _)| *m == name) {
            return Some(*kind);
        }
        if name.ends_with(".exe") && (stem.ends_with("_eac") || stem.ends_with("_eac_eos")) {
            return Some(AntiCheat::EasyAntiCheat);
        }
        if name.ends_with(".exe") && stem.ends_with("_be") {
            return Some(AntiCheat::BattlEye);
        }
    }
    None
}

fn is_library_root(dir: &Path) -> bool {
    let name = dir.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(name.as_str(), "common" | "epic games" | "games" | "xboxgames" | "windowsapps" | "riot games")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_fortnite_by_the_launchers_beside_it() {
        let root = std::env::temp_dir().join(format!("fplib-{}", std::process::id()));
        let bin = root.join("Epic Games/Fortnite/FortniteGame/Binaries/Win64");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("FortniteClient-Win64-Shipping_EAC_EOS.exe"), b"").unwrap();
        assert_eq!(anticheat(&bin.join("FortniteClient-Win64-Shipping.exe")), Some(AntiCheat::EasyAntiCheat));

        let game = root.join("steamapps/common/Quiet Game");
        std::fs::create_dir_all(game.join("bin")).unwrap();
        assert_eq!(anticheat(&game.join("bin/quiet.exe")), None);
        std::fs::create_dir_all(game.join("BattlEye")).unwrap();
        assert_eq!(anticheat(&game.join("bin/quiet.exe")), Some(AntiCheat::BattlEye));
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(anticheat(Path::new("C:/Riot Games/VALORANT/live/VALORANT.exe")), Some(AntiCheat::Kernel));
        assert!(in_game_library(Path::new("C:\\Program Files\\Epic Games\\Fortnite\\x.exe")));
        assert!(!in_game_library(Path::new("C:\\Program Files\\Google\\Chrome\\chrome.exe")));
        assert!(!is_game(Path::new(
            "C:/Program Files (x86)/Epic Games/Launcher/Portal/Binaries/Win64/EpicGamesLauncher.exe"
        )));
    }
}
