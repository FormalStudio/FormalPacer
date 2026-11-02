//! Whether the Vulkan layer is registered, and registering it for this user.

use std::io;
use std::path::{Path, PathBuf};

pub const LAYER_NAME: &str = "VK_LAYER_FORMALSTUDIO_formalpacer";
const LIBRARY: &str = "libformalpacer_layer.so";
const MANIFEST: &str = "formalpacer.json";

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn user_manifest() -> Option<PathBuf> {
    data_home().map(|d| d.join("vulkan/implicit_layer.d").join(MANIFEST))
}

fn manifest_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["VK_IMPLICIT_LAYER_PATH", "VK_ADD_IMPLICIT_LAYER_PATH"]
        .iter()
        .filter_map(std::env::var_os)
        .flat_map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .collect();
    dirs.extend(user_manifest().and_then(|m| m.parent().map(Path::to_path_buf)));
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(|d| PathBuf::from(d).join("vulkan/implicit_layer.d")));
    dirs.extend(
        ["/etc/vulkan/implicit_layer.d", "/run/current-system/sw/share/vulkan/implicit_layer.d"].map(PathBuf::from),
    );
    dirs
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerState {
    /// Registered, and the library it names exists.
    Installed(PathBuf),
    /// Registered, but the library it points at is gone (a moved or rebuilt checkout).
    Broken(PathBuf),
    Missing,
}

pub fn state() -> LayerState {
    for dir in manifest_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(json) = std::fs::read_to_string(entry.path()) else { continue };
            if !json.contains(LAYER_NAME) {
                continue;
            }
            let library = json
                .lines()
                .find_map(|l| l.trim().strip_prefix("\"library_path\":"))
                .map(|v| v.trim().trim_end_matches(',').trim_matches('"').to_owned())
                .unwrap_or_default();
            let path = if Path::new(&library).is_absolute() { PathBuf::from(&library) } else { dir.join(&library) };
            return if path.is_file() { LayerState::Installed(path) } else { LayerState::Broken(path) };
        }
    }
    LayerState::Missing
}

/// The layer library shipped with this binary: beside it in a build tree, or in `../lib`.
pub fn bundled_library() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join(LIBRARY), dir.join("../lib/formalpacer").join(LIBRARY), dir.join("../lib").join(LIBRARY)]
        .into_iter()
        .find(|p| p.is_file())
        .and_then(|p| p.canonicalize().ok())
}

pub fn manifest_json(library: &Path) -> String {
    include_str!("../../../packaging/linux/formalpacer-layer.json.in")
        .replace("@LIBRARY_PATH@", &library.display().to_string())
}

/// Writes the user-level manifest pointing at the bundled library.
pub fn install() -> io::Result<PathBuf> {
    let library = bundled_library()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{LIBRARY} is not next to this binary")))?;
    let manifest = user_manifest().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?;
    if let Some(parent) = manifest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&manifest, manifest_json(&library))?;
    Ok(manifest)
}
