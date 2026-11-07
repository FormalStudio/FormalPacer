//! `~/.config/formalpacer/config.toml`, or `%APPDATA%\FormalPacer\config.toml` on Windows.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Pick and hold a cap for every game.
    #[default]
    Auto,
    /// Measure only.
    Off,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: Mode,
    /// The connector games run on (`DP-1`), or none to follow the game.
    pub display: Option<String>,
    /// Frame rate, cap and VRR in the game's top-left corner.
    pub overlay: bool,
    /// Windows: games whose NVIDIA driver limit FormalPacer has set, with the limit each had
    /// before (0 for none), to put back when pacing is switched off.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub driver_caps: BTreeMap<String, u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self { mode: Mode::Auto, display: None, overlay: true, driver_caps: BTreeMap::new() }
    }
}

#[cfg(windows)]
fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("FormalPacer\\config.toml"))
}

#[cfg(not(windows))]
fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("formalpacer/config.toml"))
}

pub fn load() -> Config {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(config: &Config) -> std::io::Result<()> {
    let Some(path) = path() else { return Ok(()) };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string(config).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}
