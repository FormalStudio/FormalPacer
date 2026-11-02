//! Right Shift + F10 toggles the overlay. The game owns its window's input, so the key state is
//! polled from the X server on a connection of our own, the way MangoHud does it. Proton games
//! draw through XWayland, so this covers them on Wayland desktops too. libX11 is opened at
//! runtime: a game without it, or without `DISPLAY`, just has no hotkey.

use std::ffi::{c_char, c_int, c_ulong, c_void};

const XK_SHIFT_R: c_ulong = 0xffe2;
const XK_F10: c_ulong = 0xffc7;
/// Polling more often than this costs a round trip per frame for nothing.
const POLL_NS: u64 = 50_000_000;

type OpenDisplay = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type KeysymToKeycode = unsafe extern "C" fn(*mut c_void, c_ulong) -> u8;
type QueryKeymap = unsafe extern "C" fn(*mut c_void, *mut [c_char; 32]) -> c_int;

struct X11 {
    display: *mut c_void,
    query: QueryKeymap,
    keys: [u8; 2],
}

pub struct Hotkey {
    x11: Option<X11>,
    tried: bool,
    polled: u64,
    held: bool,
}

// SAFETY: the display connection is only used under the pacer's lock.
unsafe impl Send for Hotkey {}

impl Hotkey {
    pub const fn new() -> Self {
        Self { x11: None, tried: false, polled: 0, held: false }
    }

    /// True once per press of the combination.
    pub fn pressed(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.polled) < POLL_NS {
            return false;
        }
        self.polled = now;
        if !self.tried {
            self.tried = true;
            self.x11 = connect();
        }
        let Some(x11) = &self.x11 else { return false };
        let mut map = [0 as c_char; 32];
        // SAFETY: a live connection and a 32-byte buffer, as XQueryKeymap expects.
        unsafe { (x11.query)(x11.display, &mut map) };
        let down = |code: u8| map[usize::from(code / 8)] as u8 & (1 << (code % 8)) != 0;
        let held = x11.keys.iter().all(|&k| down(k));
        let pressed = held && !self.held;
        self.held = held;
        pressed
    }
}

fn connect() -> Option<X11> {
    std::env::var_os("DISPLAY")?;
    // SAFETY: dlopen/dlsym with NUL-terminated names; each symbol is cast to its Xlib signature.
    unsafe {
        let lib = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if lib.is_null() {
            return None;
        }
        let sym = |name: &std::ffi::CStr| {
            let f = libc::dlsym(lib, name.as_ptr());
            (!f.is_null()).then_some(f)
        };
        let open = std::mem::transmute::<*mut c_void, OpenDisplay>(sym(c"XOpenDisplay")?);
        let to_code = std::mem::transmute::<*mut c_void, KeysymToKeycode>(sym(c"XKeysymToKeycode")?);
        let query = std::mem::transmute::<*mut c_void, QueryKeymap>(sym(c"XQueryKeymap")?);
        let display = open(std::ptr::null());
        if display.is_null() {
            return None;
        }
        let keys = [to_code(display, XK_SHIFT_R), to_code(display, XK_F10)];
        keys.iter().all(|&k| k != 0).then_some(X11 { display, query, keys })
    }
}
