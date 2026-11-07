//! The Windows side of FormalPacer for games the Vulkan layer cannot reach: Direct3D 11, Direct3D
//! 12 and OpenGL. FormalPacer loads it into a running game, never into one under a kernel
//! anti-cheat (the app checks before loading it, and it checks again here). It hooks
//! `IDXGISwapChain::Present` and `Present1`, which every Direct3D 10 to 12 game presents through,
//! and `wglSwapBuffers` for OpenGL, sleeps before each present like the layer does, and draws the
//! overlay into the frame with clears.
//!
//! A process presents through one of them per frame. NVIDIA's OpenGL and Vulkan drivers can
//! present through DXGI underneath, so once a game has swapped through OpenGL the DXGI hook stays
//! out of it, and a process the Vulkan layer is already in is left to the layer.

#![cfg(windows)]

mod d3d11;
mod d3d12;
mod dxgi;
mod opengl;

use std::cell::Cell;
use std::sync::OnceLock;

use formalpacer_hook::game::{self, Game};
use windows::Win32::Foundation::{HINSTANCE, TRUE};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{BOOL, w};

static GAME: OnceLock<Option<Game>> = OnceLock::new();

thread_local! {
    /// Present calls in progress on this thread: one runtime's present can call another's.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

fn game() -> Option<&'static Game> {
    GAME.get_or_init(game::detect_injected).as_ref()
}

/// Runs `present` with pacing before it, unless this thread is already inside a present.
fn paced<R>(overlay: impl FnOnce(), present: impl FnOnce() -> R) -> R {
    let outer = DEPTH.with(|d| {
        let depth = d.get();
        d.set(depth + 1);
        depth == 0
    });
    if outer && let Some(game) = game() {
        formalpacer_hook::pacer::before_present(game);
        overlay();
    }
    let result = present();
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    result
}

fn install() {
    // SAFETY: looks up a module by name without loading it.
    let layer = unsafe { GetModuleHandleW(w!("formalpacer_layer.dll")) }.is_ok();
    if layer || game().is_none() {
        return;
    }
    opengl::install();
    d3d12::install();
    dxgi::install();
    // SAFETY: turns on the hooks made above, whose trampolines are already stored.
    let _ = unsafe { minhook::MinHook::enable_all_hooks() };
}

/// # Safety
/// Called by the loader. Nothing that takes the loader lock runs here: the hooks go in on a
/// thread of their own.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(_module: HINSTANCE, reason: u32, _reserved: *mut std::ffi::c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        let _ = std::thread::Builder::new().name("formalpacer".into()).spawn(install);
    }
    TRUE
}
