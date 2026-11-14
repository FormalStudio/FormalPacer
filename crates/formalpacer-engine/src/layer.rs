//! Whether the Vulkan layer is registered, and registering it for this user. On Linux the loader
//! finds manifests in `implicit_layer.d` directories; on Windows through registry values naming
//! them under `HKCU` or `HKLM\SOFTWARE\Khronos\Vulkan\ImplicitLayers`.

use std::io;
use std::path::{Path, PathBuf};

pub const LAYER_NAME: &str = "VK_LAYER_FORMALSTUDIO_formalpacer";
#[cfg(not(windows))]
const LIBRARY: &str = "libformalpacer_layer.so";
#[cfg(windows)]
const LIBRARY: &str = "formalpacer_layer.dll";
const MANIFEST: &str = "formalpacer.json";
/// The 32-bit build, for 32-bit games, in an `i686` directory beside the 64-bit one. The loader
/// skips a manifest whose `library_arch` does not match the game.
#[cfg(not(windows))]
const MANIFEST_32: &str = "formalpacer-32.json";
/// Everything a game loads from us: the layer and the `LD_PRELOAD` OpenGL hook, each also in `i686`.
#[cfg(not(windows))]
const LIBRARIES: [&str; 4] =
    [LIBRARY, "libformalpacer_gl.so", "i686/libformalpacer_layer.so", "i686/libformalpacer_gl.so"];

#[cfg(not(windows))]
fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

#[cfg(not(windows))]
fn user_manifest(name: &str) -> Option<PathBuf> {
    data_home().map(|d| d.join("vulkan/implicit_layer.d").join(name))
}

#[cfg(windows)]
fn user_manifest(name: &str) -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("FormalPacer").join(name))
}

/// Every manifest the loader would read.
#[cfg(not(windows))]
fn manifests() -> Vec<PathBuf> {
    manifest_dirs()
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()).collect::<Vec<_>>())
        .collect()
}

#[cfg(windows)]
fn manifests() -> Vec<PathBuf> {
    crate::registry::implicit_layers()
}

#[cfg(not(windows))]
fn manifest_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["VK_IMPLICIT_LAYER_PATH", "VK_ADD_IMPLICIT_LAYER_PATH"]
        .iter()
        .filter_map(std::env::var_os)
        .flat_map(|v| std::env::split_paths(&v).collect::<Vec<_>>())
        .collect();
    dirs.extend(user_manifest(MANIFEST).and_then(|m| m.parent().map(Path::to_path_buf)));
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
    for manifest in manifests() {
        let Ok(json) = std::fs::read_to_string(&manifest) else { continue };
        if !json.contains(&format!("\"{LAYER_NAME}\"")) {
            continue;
        }
        let library = json
            .lines()
            .find_map(|l| l.trim().strip_prefix("\"library_path\":"))
            .map(|v| v.trim().trim_end_matches(',').trim_matches('"').replace("\\\\", "\\"))
            .unwrap_or_default();
        let dir = manifest.parent().unwrap_or(Path::new(""));
        let path = if Path::new(&library).is_absolute() { PathBuf::from(&library) } else { dir.join(&library) };
        return if path.is_file() { LayerState::Installed(path) } else { LayerState::Broken(path) };
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

/// The manifest for the 64-bit library, or with `bits == 32` for the 32-bit one.
pub fn manifest_json(library: &Path, bits: u32) -> String {
    let name = if bits == 32 { format!("{LAYER_NAME}_32") } else { LAYER_NAME.to_owned() };
    include_str!("../../../packaging/linux/formalpacer-layer.json.in")
        .replace("@LAYER_NAME@", &name)
        .replace("@LIBRARY_PATH@", &library.display().to_string().replace('\\', "\\\\"))
        .replace("@LIBRARY_ARCH@", &bits.to_string())
}

/// Where an AppImage's libraries are copied: it runs from a mount that moves on every start and
/// is gone once the app quits, so a manifest pointing into it breaks. `None` outside an AppImage.
#[cfg(not(windows))]
fn appimage_libraries() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")?;
    data_home().map(|d| d.join("formalpacer/lib"))
}

/// Copies the libraries beside `library` into `to`, keeping the `i686` directory, and returns the
/// copy of `library`. A game may have the old copy mapped, so each file is written beside it and
/// renamed over it: the game keeps the old inode, and the next start loads the new one.
#[cfg(not(windows))]
fn copy_libraries(library: &Path, to: &Path) -> io::Result<PathBuf> {
    let from = library.parent().unwrap_or(Path::new(""));
    for name in LIBRARIES {
        let source = from.join(name);
        let Ok(bytes) = std::fs::read(&source) else { continue };
        let target = to.join(name);
        if std::fs::read(&target).is_ok_and(|current| current == bytes) {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let partial = target.with_extension("so.partial");
        std::fs::write(&partial, &bytes)?;
        std::fs::rename(&partial, &target)?;
    }
    Ok(to.join(LIBRARY))
}

/// In an AppImage, brings the copied libraries up to this version's after an update. Does
/// nothing until the layer has been registered from one.
#[cfg(not(windows))]
pub fn refresh() -> io::Result<()> {
    let (Some(to), Some(library)) = (appimage_libraries(), bundled_library()) else { return Ok(()) };
    if to.join(LIBRARY).is_file() {
        copy_libraries(&library, &to)?;
    }
    Ok(())
}

/// Velopack installs to a fixed directory and swaps its contents on update, so the manifest keeps
/// pointing at the right file.
#[cfg(windows)]
pub fn refresh() -> io::Result<()> {
    Ok(())
}

/// Writes the user-level manifests pointing at the bundled libraries, or in an AppImage at their
/// copies in the user's data directory.
#[cfg(not(windows))]
pub fn install() -> io::Result<PathBuf> {
    let mut library = bundled_library()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{LIBRARY} is not next to this binary")))?;
    if let Some(to) = appimage_libraries() {
        library = copy_libraries(&library, &to)?;
    }
    let manifest = user_manifest(MANIFEST).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no HOME"))?;
    if let Some(parent) = manifest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&manifest, manifest_json(&library, 64))?;
    if let Some(dir) = library.parent()
        && let Some(manifest_32) = user_manifest(MANIFEST_32)
    {
        let library_32 = dir.join("i686").join(LIBRARY);
        if library_32.is_file() {
            std::fs::write(manifest_32, manifest_json(&library_32, 32))?;
        }
    }
    Ok(manifest)
}

/// Writes the manifest beside the user's other FormalPacer data and registers it for this user.
/// 32-bit Vulkan games are rare enough on Windows that only the 64-bit layer is registered.
#[cfg(windows)]
pub fn install() -> io::Result<PathBuf> {
    let library = bundled_library()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{LIBRARY} is not next to this binary")))?;
    let manifest = user_manifest(MANIFEST).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no LOCALAPPDATA"))?;
    if let Some(parent) = manifest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The window shows this path: drop the verbatim prefix canonicalize adds.
    let library = PathBuf::from(library.display().to_string().trim_start_matches("\\\\?\\"));
    std::fs::write(&manifest, manifest_json(&library, 64))?;
    crate::registry::register_implicit_layer(&manifest)?;
    Ok(manifest)
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn copies_both_arches_and_replaces_a_changed_library() {
        let root = std::env::temp_dir().join(format!("formalpacer-layer-{}", std::process::id()));
        let (from, to) = (root.join("bin"), root.join("lib"));
        std::fs::create_dir_all(from.join("i686")).unwrap();
        std::fs::write(from.join(LIBRARY), b"layer 1").unwrap();
        std::fs::write(from.join("libformalpacer_gl.so"), b"gl").unwrap();
        std::fs::write(from.join("i686").join(LIBRARY), b"layer 32").unwrap();

        let copied = copy_libraries(&from.join(LIBRARY), &to).unwrap();
        assert_eq!(copied, to.join(LIBRARY));
        assert_eq!(std::fs::read(to.join(LIBRARY)).unwrap(), b"layer 1");
        assert_eq!(std::fs::read(to.join("libformalpacer_gl.so")).unwrap(), b"gl");
        assert_eq!(std::fs::read(to.join("i686").join(LIBRARY)).unwrap(), b"layer 32");
        assert!(!to.join("i686/libformalpacer_gl.so").exists());

        std::fs::write(from.join(LIBRARY), b"layer 2").unwrap();
        copy_libraries(&from.join(LIBRARY), &to).unwrap();
        assert_eq!(std::fs::read(to.join(LIBRARY)).unwrap(), b"layer 2");
        assert!(!to.join("libformalpacer_layer.so.partial").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
