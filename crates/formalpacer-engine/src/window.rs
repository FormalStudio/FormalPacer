//! A Windows process's game window: the foreground window when it is the process's, else its
//! largest visible top-level window. A browser or an Electron app presents from a GPU process that
//! owns no window, so asking for the presenting process's window leaves them out. Only window handles and their geometry are read; nothing is
//! opened in the game's process.

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};

fn owner(window: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: writes the owning process id of a window handle.
    unsafe { GetWindowThreadProcessId(window, &mut pid) };
    pid
}

fn client_size(window: HWND) -> (u32, u32) {
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: writes the window's client rectangle.
    unsafe { GetClientRect(window, &mut rect) };
    ((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32)
}

/// The process that owns the foreground window.
pub fn foreground_pid() -> u32 {
    // SAFETY: reads the foreground window handle.
    let window = unsafe { GetForegroundWindow() };
    if window.is_null() { 0 } else { owner(window) }
}

/// Whether the process's window covers its whole monitor: a fullscreen or borderless game.
pub fn fills_monitor(pid: u32) -> bool {
    let Some(window) = game_window(pid) else { return false };
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: a window handle in; a monitor handle out, never null with DEFAULTTONEAREST.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    // SAFETY: plain data, valid when zeroed.
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: writes the window's rectangle and a sized MONITORINFOEXW.
    let ok = unsafe { GetWindowRect(window, &mut rect) != 0 && GetMonitorInfoW(monitor, (&raw mut info).cast()) != 0 };
    let m = info.monitorInfo.rcMonitor;
    ok && rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
}

pub fn game_window(pid: u32) -> Option<HWND> {
    // SAFETY: reads the foreground window handle.
    let foreground = unsafe { GetForegroundWindow() };
    if !foreground.is_null() && owner(foreground) == pid {
        return Some(foreground);
    }
    struct Search {
        pid: u32,
        best: Option<(HWND, u64)>,
    }
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data` is the Search below, alive for the whole enumeration.
        let search = unsafe { &mut *(data as *mut Search) };
        // SAFETY: plain queries on a window handle the enumeration gave us.
        let shown = unsafe { IsWindowVisible(window) != 0 && IsIconic(window) == 0 };
        if shown && owner(window) == search.pid {
            let (w, h) = client_size(window);
            let area = u64::from(w) * u64::from(h);
            if search.best.is_none_or(|(_, a)| area > a) {
                search.best = Some((window, area));
            }
        }
        1
    }
    let mut search = Search { pid, best: None };
    // SAFETY: the callback only touches `search`, which outlives the call.
    unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
    search.best.map(|(w, _)| w)
}

/// The GDI name (`DISPLAY1`) of the monitor holding most of the game's window.
pub fn game_output(pid: u32, _x11_window: u32) -> Option<String> {
    let window = game_window(pid)?;
    // SAFETY: a window handle in; a monitor handle out, never null with DEFAULTTONEAREST.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    // SAFETY: plain data, valid when zeroed.
    let mut info: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: a sized MONITORINFOEXW to fill.
    if unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast()) } == 0 {
        return None;
    }
    let end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
    Some(String::from_utf16_lossy(&info.szDevice[..end]).trim_start_matches("\\\\.\\").to_owned())
}

/// The size of the game's window, standing in for its swapchain when nothing inside the game
/// reports one.
pub fn game_extent(pid: u32) -> (u32, u32) {
    game_window(pid).map_or((0, 0), client_size)
}
