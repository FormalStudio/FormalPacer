//! Which display a game's window is on, asked of whoever knows. On Linux: the compositor for the
//! game's process (Hyprland, Sway, niri), or for an X11 or XWayland window the X server's RandR
//! monitors, using the window the layer saw the game create its swapchain surface for. Every
//! answer is a connector name (`DP-3`), the same name the kernel uses. On Windows: the monitor
//! holding the game's window, by its GDI name (`DISPLAY1`).

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;

use serde_json::Value;
#[cfg(unix)]
use x11rb::connection::Connection;
#[cfg(unix)]
use x11rb::protocol::randr::ConnectionExt as _;
#[cfg(unix)]
use x11rb::protocol::xproto::ConnectionExt as _;

#[cfg(unix)]
const TIMEOUT: Duration = Duration::from_millis(200);

#[cfg(unix)]
pub fn game_output(pid: u32, x11_window: u32) -> Option<String> {
    hyprland(pid)
        .or_else(|| sway(pid))
        .or_else(|| niri(pid))
        .or_else(|| (x11_window != 0).then(|| x11(x11_window)).flatten())
}

#[cfg(windows)]
pub use crate::window::game_output;

#[cfg(unix)]
fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

#[cfg(unix)]
fn request(path: &PathBuf, payload: &[u8]) -> Option<Vec<u8>> {
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
    stream.write_all(payload).ok()?;
    let mut out = Vec::new();
    stream.read_to_end(&mut out).ok()?;
    Some(out)
}

/// The instance this app runs under, or the only one running when the app was started from
/// somewhere that did not pass the variable on.
#[cfg(unix)]
fn hyprland_socket() -> Option<PathBuf> {
    let dir = runtime_dir()?.join("hypr");
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().or_else(|| {
        let mut entries = std::fs::read_dir(&dir).ok()?.flatten().filter(|e| e.path().join(".socket.sock").exists());
        let only = entries.next()?;
        entries.next().is_none().then(|| only.file_name().to_string_lossy().into_owned())
    })?;
    Some(dir.join(sig).join(".socket.sock"))
}

#[cfg(unix)]
fn hyprland(pid: u32) -> Option<String> {
    let socket = hyprland_socket()?;
    let clients: Value = serde_json::from_slice(&request(&socket, b"j/clients")?).ok()?;
    let monitors: Value = serde_json::from_slice(&request(&socket, b"j/monitors")?).ok()?;
    hyprland_pick(&clients, &monitors, pid)
}

/// The game's most recently focused window decides, which is the one being played when a game
/// also has a launcher window open.
pub fn hyprland_pick(clients: &Value, monitors: &Value, pid: u32) -> Option<String> {
    let client = clients
        .as_array()?
        .iter()
        .filter(|c| c["pid"].as_u64() == Some(u64::from(pid)))
        .min_by_key(|c| c["focusHistoryID"].as_i64().unwrap_or(i64::MAX))?;
    let id = client["monitor"].as_i64()?;
    monitors.as_array()?.iter().find(|m| m["id"].as_i64() == Some(id))?["name"].as_str().map(str::to_owned)
}

#[cfg(unix)]
fn sway(pid: u32) -> Option<String> {
    let socket = PathBuf::from(std::env::var_os("SWAYSOCK")?);
    // i3 IPC: magic, payload length, message type 4 (GET_TREE), no payload.
    let mut msg = b"i3-ipc".to_vec();
    msg.extend(0u32.to_ne_bytes());
    msg.extend(4u32.to_ne_bytes());
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    stream.write_all(&msg).ok()?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).ok()?;
    let len = u32::from_ne_bytes(header[6..10].try_into().ok()?) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    let tree: Value = serde_json::from_slice(&body).ok()?;
    sway_pick(&tree, pid)
}

pub fn sway_pick(tree: &Value, pid: u32) -> Option<String> {
    fn has_pid(node: &Value, pid: u32) -> bool {
        node["pid"].as_u64() == Some(u64::from(pid))
            || ["nodes", "floating_nodes"]
                .iter()
                .any(|k| node[*k].as_array().is_some_and(|n| n.iter().any(|c| has_pid(c, pid))))
    }
    tree["nodes"]
        .as_array()?
        .iter()
        .filter(|o| o["type"] == "output" && o["name"] != "__i3")
        .find(|o| has_pid(o, pid))?["name"]
        .as_str()
        .map(str::to_owned)
}

#[cfg(unix)]
fn niri(pid: u32) -> Option<String> {
    let socket = PathBuf::from(std::env::var_os("NIRI_SOCKET")?);
    let windows: Value = serde_json::from_slice(&request(&socket, b"\"Windows\"\n")?).ok()?;
    let workspaces: Value = serde_json::from_slice(&request(&socket, b"\"Workspaces\"\n")?).ok()?;
    let window = windows["Ok"]["Windows"].as_array()?.iter().find(|w| w["pid"].as_u64() == Some(u64::from(pid)))?;
    let ws = window["workspace_id"].as_u64()?;
    workspaces["Ok"]["Workspaces"].as_array()?.iter().find(|w| w["id"].as_u64() == Some(ws))?["output"]
        .as_str()
        .map(str::to_owned)
}

/// The RandR monitor holding the window's centre. XWayland names its monitors after the
/// Wayland outputs, which are the connector names.
#[cfg(unix)]
fn x11(window: u32) -> Option<String> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots.get(screen)?.root;
    let geo = conn.get_geometry(window).ok()?.reply().ok()?;
    let at = conn.translate_coordinates(window, root, 0, 0).ok()?.reply().ok()?;
    let (cx, cy) = (i32::from(at.dst_x) + i32::from(geo.width) / 2, i32::from(at.dst_y) + i32::from(geo.height) / 2);
    let monitors = conn.randr_get_monitors(root, true).ok()?.reply().ok()?;
    let monitor = monitors.monitors.iter().find(|m| {
        let (x, y) = (i32::from(m.x), i32::from(m.y));
        cx >= x && cx < x + i32::from(m.width) && cy >= y && cy < y + i32::from(m.height)
    })?;
    let name = conn.get_atom_name(monitor.name).ok()?.reply().ok()?;
    String::from_utf8(name.name).ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn hyprland_uses_the_games_focused_window() {
        let monitors = json!([{ "id": 0, "name": "HDMI-A-1" }, { "id": 1, "name": "DP-3" }]);
        let clients = json!([
            { "pid": 10, "monitor": 0, "focusHistoryID": 0 },
            { "pid": 42, "monitor": 0, "focusHistoryID": 5 },
            { "pid": 42, "monitor": 1, "focusHistoryID": 1 },
        ]);
        assert_eq!(hyprland_pick(&clients, &monitors, 42).as_deref(), Some("DP-3"));
        assert_eq!(hyprland_pick(&clients, &monitors, 7), None);
    }

    #[test]
    fn sway_finds_the_output_holding_the_pid() {
        let tree = json!({ "nodes": [
            { "type": "output", "name": "__i3", "nodes": [] },
            { "type": "output", "name": "DP-1", "nodes": [{ "nodes": [{ "pid": 3 }] }] },
            { "type": "output", "name": "DP-2", "nodes": [{ "nodes": [], "floating_nodes": [{ "pid": 42 }] }] },
        ]});
        assert_eq!(sway_pick(&tree, 42).as_deref(), Some("DP-2"));
    }
}
