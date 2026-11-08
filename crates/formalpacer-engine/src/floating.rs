//! The overlay for a game nothing of FormalPacer's runs inside, such as one under a kernel
//! anti-cheat: a window of our own over the top-left corner of the game's monitor, always on top,
//! never focused and transparent to clicks. It shows the picture the in-game overlay draws. Only
//! our window is touched, never the game's process.

use formalpacer_hook::overlay::{BACKGROUND, Bitmap, COLORS, MARGIN, Text, scale_for};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, HWND_TOPMOST, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, ShowWindow, ULW_ALPHA,
    UpdateLayeredWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

/// How often the window's thread looks for a new picture and handles its messages.
const POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// What to show, and the monitor (`left, top, height`) to show it on.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub text: Text,
    pub monitor: (i32, i32, u32),
}

pub struct Floating {
    updates: flume::Sender<Option<Shown>>,
    last: Option<Shown>,
}

impl Floating {
    pub fn start() -> Self {
        let (updates, rx) = flume::unbounded();
        // Without its thread there is no window; the engine paces all the same.
        let _ = std::thread::Builder::new().name("formalpacer-overlay".into()).spawn(move || run(&rx));
        Self { updates, last: None }
    }

    pub fn show(&mut self, shown: Option<Shown>) {
        if self.last != shown {
            let _ = self.updates.send(shown.clone());
            self.last = shown;
        }
    }
}

unsafe extern "system" fn window_proc(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: the default handling; the window draws only through UpdateLayeredWindow.
    unsafe { DefWindowProcW(window, message, w, l) }
}

fn create() -> Option<HWND> {
    let class: Vec<u16> = "FormalPacerOverlay\0".encode_utf16().collect();
    // SAFETY: registers a class of our own and creates a hidden popup of it, owned by this thread.
    unsafe {
        let instance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            // SAFETY: the remaining fields are handles and flags, valid when zeroed.
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        (!window.is_null()).then_some(window)
    }
}

fn run(updates: &flume::Receiver<Option<Shown>>) {
    let Some(window) = create() else { return };
    loop {
        match updates.recv_timeout(POLL) {
            Ok(Some(shown)) => draw(window, &shown),
            // SAFETY: hides our own window.
            Ok(None) => unsafe {
                ShowWindow(window, SW_HIDE);
            },
            Err(flume::RecvTimeoutError::Timeout) => {}
            Err(flume::RecvTimeoutError::Disconnected) => return,
        }
        // SAFETY: drains this thread's queue into the default window procedure.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                DispatchMessageW(&msg);
            }
        }
    }
}

/// The picture as premultiplied BGRA, the layout a layered window takes. The box is a little
/// see-through; the text is not.
fn pixels(bitmap: &Bitmap) -> Vec<u32> {
    bitmap
        .pixels
        .iter()
        .map(|&value| {
            let [r, g, b, _] = COLORS[usize::from(value)];
            let a: u32 = if value == BACKGROUND { 216 } else { 255 };
            let pre = |c: u8| u32::from(c) * a / 255;
            (a << 24) | (pre(r) << 16) | (pre(g) << 8) | pre(b)
        })
        .collect()
}

fn draw(window: HWND, shown: &Shown) {
    let (left, top, height) = shown.monitor;
    let scale = scale_for(height);
    let bitmap = Bitmap::render(&shown.text, scale);
    let (w, h) = (bitmap.width as i32, bitmap.height as i32);
    let margin = (MARGIN * scale) as i32;
    let source = pixels(&bitmap);
    // SAFETY: a memory DC and a top-down 32-bit DIB of exactly the picture's size, filled from
    // `source`, handed to our own window, then released.
    unsafe {
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return;
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if !dib.is_null() && !bits.is_null() {
            std::ptr::copy_nonoverlapping(source.as_ptr(), bits.cast::<u32>(), source.len());
            let previous = SelectObject(dc, dib);
            let at = POINT { x: left + margin, y: top + margin };
            let size = SIZE { cx: w, cy: h };
            let origin = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(window, std::ptr::null_mut(), &at, &size, dc, &origin, 0, &blend, ULW_ALPHA);
            SelectObject(dc, previous);
            // A borderless game can be topmost too: stay above it.
            SetWindowPos(window, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            ShowWindow(window, SW_SHOWNOACTIVATE);
        }
        if !dib.is_null() {
            DeleteObject(dib);
        }
        DeleteDC(dc);
    }
}

/// The monitor holding most of the game's window, as `left, top, height`.
pub fn monitor_of(pid: u32) -> Option<(i32, i32, u32)> {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    let window = crate::window::game_window(pid)?;
    // SAFETY: a window handle in; a monitor handle out, never null with DEFAULTTONEAREST.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    // SAFETY: plain data, valid when zeroed.
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    // SAFETY: a sized MONITORINFO to fill.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let RECT { left, top, bottom, .. } = info.rcMonitor;
    Some((left, top, (bottom - top).max(0) as u32))
}
