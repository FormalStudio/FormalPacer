//! Steam game names from the app manifests in the user's Steam libraries.

use std::path::PathBuf;

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
            libraries.extend(vdf.lines().filter_map(|l| vdf_value(l, "path")).map(PathBuf::from));
        }
    }
    libraries.iter().find_map(|lib| {
        let acf = std::fs::read_to_string(lib.join(format!("steamapps/appmanifest_{app_id}.acf"))).ok()?;
        acf.lines().find_map(|l| vdf_value(l, "name")).map(str::to_owned)
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
