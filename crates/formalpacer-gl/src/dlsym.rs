//! `dlsym` hands back our swap functions for games that look them up in a libGL or libEGL they
//! opened themselves. Every other lookup is passed on by a tail jump to the next `dlsym`, so the
//! real one sees the caller's return address: `RTLD_NEXT` resolves relative to the object that
//! asked, and another preloaded library looking up the next `dlopen` or `glXSwapBuffers` does not
//! get its own back.
//!
//! The next `dlsym` is the one after us in the preload order, not glibc's. Steam's overlay
//! exports its own and sets up its other hooks only when that one is first called; bypassed, its
//! `close` stays a no-op, and every `$(...)` in Steam's launch scripts waits on a pipe that never
//! closes.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::{Dlsym, game, hook};

static REAL: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static NEXT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

#[cfg(target_arch = "x86_64")]
const OLD_VERSION: &CStr = c"GLIBC_2.2.5";
#[cfg(target_arch = "x86")]
const OLD_VERSION: &CStr = c"GLIBC_2.0";

/// glibc's own `dlsym`. `dlvsym` is not interposed, so finding it cannot come back here.
extern "C" fn real_ptr() -> *mut c_void {
    let ptr = REAL.load(Ordering::Relaxed);
    if !ptr.is_null() {
        return ptr;
    }
    // glibc 2.34 moved dlsym from libdl into libc under a new version.
    let ptr = [c"GLIBC_2.34", OLD_VERSION]
        .iter()
        // SAFETY: RTLD_NEXT and NUL-terminated name and version.
        .map(|version| unsafe { libc::dlvsym(libc::RTLD_NEXT, c"dlsym".as_ptr(), version.as_ptr()) })
        .find(|f| !f.is_null())
        .unwrap_or(no_dlsym as *mut c_void);
    REAL.store(ptr, Ordering::Relaxed);
    ptr
}

extern "C" fn no_dlsym(_: *mut c_void, _: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

/// The `dlsym` after ours in symbol order: another interposer's, else glibc's.
extern "C" fn next_ptr() -> *mut c_void {
    let ptr = NEXT.load(Ordering::Relaxed);
    if !ptr.is_null() {
        return ptr;
    }
    // SAFETY: RTLD_NEXT and a NUL-terminated name, resolved after this library.
    let next = unsafe { real_dlsym()(libc::RTLD_NEXT, c"dlsym".as_ptr()) };
    let ptr = if next.is_null() || next == dlsym as *mut c_void { real_ptr() } else { next };
    NEXT.store(ptr, Ordering::Relaxed);
    ptr
}

fn next_dlsym() -> Dlsym {
    // SAFETY: next_ptr returns a dlsym.
    unsafe { std::mem::transmute::<*mut c_void, Dlsym>(next_ptr()) }
}

pub fn real_dlsym() -> Dlsym {
    // SAFETY: real_ptr returns dlsym itself or a function of the same type.
    unsafe { std::mem::transmute::<*mut c_void, Dlsym>(real_ptr()) }
}

/// Our function for `name` if we take it over, else null to pass the call on. `RTLD_NEXT`
/// lookups come from other interposers asking for what sits below them, never from the game.
extern "C" fn route(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    if handle == libc::RTLD_NEXT || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: dlsym callers pass a NUL-terminated name.
    let name = unsafe { CStr::from_ptr(name) };
    // The name check comes first: game detection allocates, and allocators look symbols up too.
    let Some((ours, slot)) = hook(name) else { return std::ptr::null_mut() };
    if game().is_none() {
        return std::ptr::null_mut();
    }
    // Through the next dlsym, so an overlay behind us keeps its own swap hook and we call into it.
    // SAFETY: the caller's own handle and name.
    let found = unsafe { next_dlsym()(handle, name.as_ptr()) };
    if found.is_null() {
        return std::ptr::null_mut();
    }
    slot.remember(found, ours);
    ours
}

/// # Safety
/// The caller's contract for `dlsym`.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    core::arch::naked_asm!(
        "push rdi",
        "push rsi",
        "sub rsp, 8",
        "call {route}",
        "test rax, rax",
        "jnz 2f",
        "call {next}",
        "add rsp, 8",
        "pop rsi",
        "pop rdi",
        "jmp rax",
        "2:",
        "add rsp, 8",
        "pop rsi",
        "pop rdi",
        "ret",
        route = sym route,
        next = sym next_ptr,
    )
}

/// # Safety
/// The caller's contract for `dlsym`.
#[cfg(target_arch = "x86")]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void {
    // A push with an esp-based source reads before esp moves, so each push copies the argument
    // 12 bytes up; the first `sub` keeps esp 16-byte aligned at the calls.
    core::arch::naked_asm!(
        "sub esp, 4",
        "push dword ptr [esp + 12]",
        "push dword ptr [esp + 12]",
        "call {route}",
        "add esp, 12",
        "test eax, eax",
        "jnz 2f",
        "sub esp, 12",
        "call {next}",
        "add esp, 12",
        "jmp eax",
        "2:",
        "ret",
        route = sym route,
        next = sym next_ptr,
    )
}
