//! `wglSwapBuffers`, which `SwapBuffers` in GDI calls for OpenGL windows. The overlay is the
//! scissored clears the Linux hook draws, with functions from `opengl32.dll` and the driver.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

use formalpacer_hook::gl::Gl;
use formalpacer_hook::pacer;
use minhook::MinHook;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{HDC, WindowFromDC};
use windows::Win32::Graphics::OpenGL::wglGetProcAddress;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, PCSTR, s, w};

type SwapBuffers = unsafe extern "system" fn(HDC) -> BOOL;

static ORIGINAL: OnceLock<SwapBuffers> = OnceLock::new();
static GL: OnceLock<Option<Gl>> = OnceLock::new();

/// Only a game that already uses OpenGL has `opengl32.dll` loaded; nothing loads it for one that
/// does not.
pub fn install() {
    // SAFETY: looks up a loaded module and one of its exports; MinHook patches it and returns a
    // trampoline to the original, stored before the hook is enabled.
    unsafe {
        let Ok(module) = GetModuleHandleW(w!("opengl32.dll")) else { return };
        let Some(target) = GetProcAddress(module, s!("wglSwapBuffers")) else { return };
        if let Ok(original) = MinHook::create_hook(target as *mut c_void, hooked as *mut c_void) {
            let _ = ORIGINAL.set(std::mem::transmute::<*mut c_void, SwapBuffers>(original));
        }
    }
}

/// OpenGL 1.1 comes from `opengl32.dll`, everything newer from the driver through
/// `wglGetProcAddress`, which some drivers answer with 1, 2, 3 or -1 for a name they lack.
fn lookup(name: &CStr) -> *mut c_void {
    let name = PCSTR(name.as_ptr().cast());
    // SAFETY: a NUL-terminated name; a context is current while the hook draws.
    unsafe {
        if let Ok(module) = GetModuleHandleW(w!("opengl32.dll"))
            && let Some(f) = GetProcAddress(module, name)
        {
            return f as *mut c_void;
        }
        let f = wglGetProcAddress(name).map_or(std::ptr::null_mut(), |f| f as *mut c_void);
        if matches!(f.addr(), 1..=3 | usize::MAX) { std::ptr::null_mut() } else { f }
    }
}

fn draw(dc: HDC) {
    let mut rect = RECT::default();
    // SAFETY: the window of the device context being swapped.
    if unsafe { GetClientRect(WindowFromDC(dc), &mut rect) }.is_err() {
        return;
    }
    let (width, height) = ((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32);
    pacer::note_extent(width, height);
    let Some(frame) = pacer::overlay(height) else { return };
    let Some(gl) = GL.get_or_init(|| Gl::load(lookup)) else { return };
    // SAFETY: the game's context is current on this thread and its drawable is the one swapped.
    unsafe { gl.draw(&frame, height) };
}

unsafe extern "system" fn hooked(dc: HDC) -> BOOL {
    let Some(&original) = ORIGINAL.get() else { return BOOL(0) };
    crate::dxgi::UNDER_OPENGL.store(true, Ordering::Relaxed);
    // SAFETY: the game's own call, passed on unchanged.
    crate::paced(|| draw(dc), || unsafe { original(dc) })
}
