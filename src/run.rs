//! `formalpacer run <command>`: starts a game with the OpenGL hook preloaded. Vulkan games need
//! none of this; the layer finds them by itself. Both the 64-bit and the 32-bit hook go into
//! `LD_PRELOAD`, and the dynamic linker skips the one of the wrong class with a warning, the
//! way Steam loads its overlay.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;

const LIBRARY: &str = "libformalpacer_gl.so";

/// The hooks shipped with this binary: installed in `../lib/formalpacer`, or a build tree.
fn hooks() -> Vec<PathBuf> {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) else {
        return Vec::new();
    };
    let installed = dir.join("../lib/formalpacer");
    [
        installed.join(LIBRARY),
        installed.join("i686").join(LIBRARY),
        dir.join(LIBRARY),
        dir.join("../i686-unknown-linux-gnu").join(dir.file_name().unwrap_or_default()).join(LIBRARY),
    ]
    .into_iter()
    .filter_map(|p| p.canonicalize().ok())
    .collect()
}

/// Replaces this process with `command`; only returns on failure.
pub fn exec(command: &[String]) -> std::io::Error {
    let Some((program, args)) = command.split_first() else {
        return std::io::Error::new(std::io::ErrorKind::InvalidInput, "usage: formalpacer run <command> [args]");
    };
    let mut preload: Vec<String> = hooks().iter().map(|p| p.display().to_string()).collect();
    if preload.is_empty() {
        return std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{LIBRARY} is not installed next to formalpacer"),
        );
    }
    if let Some(existing) = std::env::var_os("LD_PRELOAD") {
        preload.push(existing.to_string_lossy().into_owned());
    }
    std::process::Command::new(program).args(args).env("LD_PRELOAD", preload.join(" ")).exec()
}
