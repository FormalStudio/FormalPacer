//! Game names from the stores' install manifests: Steam's app manifests in the user's Steam
//! libraries, and on Windows the Epic Games launcher's.

use std::path::PathBuf;

#[cfg(windows)]
fn steam_roots() -> Vec<PathBuf> {
    crate::registry::user_string("Software\\Valve\\Steam", "SteamPath")
        .map(PathBuf::from)
        .into_iter()
        .chain([PathBuf::from("C:\\Program Files (x86)\\Steam")])
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(not(windows))]
fn steam_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    [".local/share/Steam", ".steam/steam", ".var/app/com.valvesoftware.Steam/data/Steam"]
        .iter()
        .map(|p| home.join(p))
        .filter(|p| p.is_dir())
        .collect()
}

/// The quoted value after `"key"` on a VDF line.
fn vdf_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.trim().strip_prefix(&format!("\"{key}\""))?;
    let rest = rest.trim().strip_prefix('"')?;
    rest.strip_suffix('"')
}

pub fn app_name(app_id: u32) -> Option<String> {
    let mut libraries = Vec::new();
    for root in steam_roots() {
        libraries.push(root.clone());
        if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            // VDF escapes backslashes, which only Windows paths have.
            libraries.extend(
                vdf.lines().filter_map(|l| vdf_value(l, "path")).map(|p| PathBuf::from(p.replace("\\\\", "\\"))),
            );
        }
    }
    libraries.iter().find_map(|lib| {
        let acf = std::fs::read_to_string(lib.join(format!("steamapps/appmanifest_{app_id}.acf"))).ok()?;
        acf.lines().find_map(|l| vdf_value(l, "name")).map(str::to_owned)
    })
}

/// The name of the game installed around `exe`, for a game found by its executable.
#[cfg(windows)]
pub fn app_name_for(exe: &std::path::Path) -> Option<String> {
    steam_name_for(exe).or_else(|| epic_name_for(exe))
}

/// `<library>\steamapps\common\<installdir>\...`: the manifest in `<library>\steamapps` with that
/// `installdir`.
#[cfg(windows)]
fn steam_name_for(exe: &std::path::Path) -> Option<String> {
    let parts: Vec<_> = exe.components().collect();
    let at = parts.iter().position(|c| c.as_os_str().eq_ignore_ascii_case("steamapps"))?;
    let install_dir = parts.get(at + 2)?.as_os_str().to_string_lossy().into_owned();
    let steamapps: PathBuf = parts[..=at].iter().collect();
    std::fs::read_dir(steamapps).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
            return None;
        }
        let acf = std::fs::read_to_string(e.path()).ok()?;
        let dir = acf.lines().find_map(|l| vdf_value(l, "installdir"))?;
        dir.eq_ignore_ascii_case(&install_dir)
            .then(|| acf.lines().find_map(|l| vdf_value(l, "name")).map(str::to_owned))
            .flatten()
    })
}

/// The launcher keeps one JSON manifest per installed game, with its display name and install
/// location.
#[cfg(windows)]
fn epic_name_for(exe: &std::path::Path) -> Option<String> {
    let manifests = PathBuf::from(std::env::var_os("ProgramData")?).join("Epic\\EpicGamesLauncher\\Data\\Manifests");
    let exe = exe.to_string_lossy().to_ascii_lowercase();
    std::fs::read_dir(manifests).ok()?.flatten().find_map(|e| {
        let item: serde_json::Value = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
        let location = item["InstallLocation"].as_str()?.to_ascii_lowercase();
        (!location.is_empty() && exe.starts_with(&location))
            .then(|| item["DisplayName"].as_str().map(str::to_owned))
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_vdf_values() {
        assert_eq!(super::vdf_value("\t\"name\"\t\t\"ELDEN RING\"", "name"), Some("ELDEN RING"));
        assert_eq!(super::vdf_value("\t\"path\"\t\t\"/mnt/games\"", "name"), None);
    }
}
