//! The OpenGL side of FormalPacer: an `LD_PRELOAD` library that wraps `glXSwapBuffers` and
//! `eglSwapBuffers`. Before each swap it sleeps until the next deadline of the interval the app
//! asked for, records the frame, and clears the overlay into the back buffer. It is loaded only
//! by `formalpacer run`, so every process it lands in is treated as a game.
//!
//! Games reach the swap functions three ways, and each is covered: linking libGL or libEGL
//! directly (our exports come first), `glXGetProcAddress`/`eglGetProcAddress`, and
//! `dlsym(dlopen("libGL.so.1"), ...)`, which SDL and most engines use.

// Windows has its own hook for OpenGL and Direct3D games: `crates/formalpacer-inject`.
#![cfg(unix)]

/// Looks up `$name` with `$lookup` as a `$ty`, returning `None` from the caller if it is missing.
macro_rules! load {
    ($lookup:expr, $name:literal, $ty:ty) => {{
        let f: *mut std::ffi::c_void = $lookup($name);
        if f.is_null() {
            return None;
        }
        // SAFETY: the driver hands back the entry point of the name asked for, of that type.
        unsafe { std::mem::transmute::<*mut std::ffi::c_void, $ty>(f) }
    }};
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod dlsym;

use std::ffi::{CStr, c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use formalpacer_hook::gl::Gl;
use formalpacer_hook::game::{self, Game};
use formalpacer_hook::pacer;

type Dlsym = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void;
type ProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type GlxSwapBuffers = unsafe extern "C" fn(*mut c_void, c_ulong);
type GlxGetCurrentDrawable = unsafe extern "C" fn() -> c_ulong;
type GlxQueryDrawable = unsafe extern "C" fn(*mut c_void, c_ulong, c_int, *mut c_uint);
type EglSwapBuffers = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_uint;
type EglSwapBuffersWithDamage = unsafe extern "C" fn(*mut c_void, *mut c_void, *const i32, i32) -> c_uint;
type EglGetCurrentSurface = unsafe extern "C" fn(i32) -> *mut c_void;
type EglQuerySurface = unsafe extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> c_uint;

const GLX_WIDTH: c_int = 0x801D;
const GLX_HEIGHT: c_int = 0x801E;
const EGL_HEIGHT: i32 = 0x3056;
const EGL_WIDTH: i32 = 0x3057;
const EGL_DRAW: i32 = 0x3059;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use dlsym::real_dlsym;

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn real_dlsym() -> Dlsym {
    libc::dlsym
}

static ACTIVE: OnceLock<Option<Game>> = OnceLock::new();

fn game() -> Option<&'static Game> {
    ACTIVE.get_or_init(game::detect_injected).as_ref()
}

/// Where the driver's own function behind one of our exports lives.
struct Real {
    name: &'static CStr,
    ptr: AtomicPtr<c_void>,
}

impl Real {
    const fn new(name: &'static CStr) -> Self {
        Self { name, ptr: AtomicPtr::new(std::ptr::null_mut()) }
    }

    /// A lookup in the global scope finds our export first, which must never be called as the real one.
    fn remember(&self, found: *mut c_void, ours: *mut c_void) {
        if !found.is_null() && found != ours {
            self.ptr.store(found, Ordering::Relaxed);
        }
    }

    /// The function from the handle the game looked it up in, else the next object's after us.
    fn get(&self, ours: *mut c_void) -> *mut c_void {
        let ptr = self.ptr.load(Ordering::Relaxed);
        if !ptr.is_null() {
            return ptr;
        }
        // SAFETY: RTLD_NEXT and a NUL-terminated name; the lookup starts after this library.
        self.remember(unsafe { real_dlsym()(libc::RTLD_NEXT, self.name.as_ptr()) }, ours);
        self.ptr.load(Ordering::Relaxed)
    }
}

static GLX_SWAP: Real = Real::new(c"glXSwapBuffers");
static GLX_PROC: Real = Real::new(c"glXGetProcAddress");
static GLX_PROC_ARB: Real = Real::new(c"glXGetProcAddressARB");
static EGL_SWAP: Real = Real::new(c"eglSwapBuffers");
static EGL_SWAP_DAMAGE_KHR: Real = Real::new(c"eglSwapBuffersWithDamageKHR");
static EGL_SWAP_DAMAGE_EXT: Real = Real::new(c"eglSwapBuffersWithDamageEXT");
static EGL_PROC: Real = Real::new(c"eglGetProcAddress");

/// Our export and the slot for the real function, for each name we take over.
fn hook(name: &CStr) -> Option<(*mut c_void, &'static Real)> {
    Some(match name.to_bytes() {
        b"glXSwapBuffers" => (glXSwapBuffers as *mut c_void, &GLX_SWAP),
        b"glXGetProcAddress" => (glXGetProcAddress as *mut c_void, &GLX_PROC),
        b"glXGetProcAddressARB" => (glXGetProcAddressARB as *mut c_void, &GLX_PROC_ARB),
        b"eglSwapBuffers" => (eglSwapBuffers as *mut c_void, &EGL_SWAP),
        b"eglSwapBuffersWithDamageKHR" => (eglSwapBuffersWithDamageKHR as *mut c_void, &EGL_SWAP_DAMAGE_KHR),
        b"eglSwapBuffersWithDamageEXT" => (eglSwapBuffersWithDamageEXT as *mut c_void, &EGL_SWAP_DAMAGE_EXT),
        b"eglGetProcAddress" => (eglGetProcAddress as *mut c_void, &EGL_PROC),
        _ => return None,
    })
}

/// `name` from the library that holds `known`. A game that opened libGL with `RTLD_LOCAL` and
/// looked up only the swap function leaves `RTLD_NEXT` nothing to find.
fn beside(known: *mut c_void, name: &CStr) -> *mut c_void {
    if known.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: dladdr fills a zeroed Dl_info; the library is already loaded, so RTLD_NOLOAD only
    // takes a reference, which dlclose gives back.
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(known, &mut info) == 0 || info.dli_fname.is_null() {
            return std::ptr::null_mut();
        }
        let lib = libc::dlopen(info.dli_fname, libc::RTLD_NOW | libc::RTLD_NOLOAD);
        if lib.is_null() {
            return std::ptr::null_mut();
        }
        let f = real_dlsym()(lib, name.as_ptr());
        libc::dlclose(lib);
        f
    }
}

/// The driver's `GetProcAddress` behind `ours`, for a context using `swap`.
fn real_proc_address(slot: &Real, ours: *mut c_void, swap: &Real) -> Option<ProcAddress> {
    let mut f = slot.get(ours);
    if f.is_null() {
        f = beside(swap.ptr.load(Ordering::Relaxed), slot.name);
    }
    // SAFETY: a GetProcAddress entry point, found under its own name.
    (!f.is_null() && f != ours).then(|| unsafe { std::mem::transmute::<*mut c_void, ProcAddress>(f) })
}

/// # Safety
/// `name` is null or NUL-terminated, as for the driver's own `GetProcAddress`.
unsafe fn get_proc_address(slot: &Real, ours: *mut c_void, name: *const c_char) -> *mut c_void {
    let real = slot.get(ours);
    if real.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: forwarding the game's own call to the driver's GetProcAddress.
    let found = unsafe { std::mem::transmute::<*mut c_void, ProcAddress>(real)(name) };
    if found.is_null() || game().is_none() {
        return found;
    }
    // SAFETY: checked non-null; the caller passes a NUL-terminated name.
    match hook(unsafe { CStr::from_ptr(name) }) {
        Some((ours, slot)) => {
            slot.remember(found, ours);
            ours
        }
        None => found,
    }
}

/// # Safety
/// `name` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddress(name: *const c_char) -> *mut c_void {
    // SAFETY: the caller's contract.
    unsafe { get_proc_address(&GLX_PROC, glXGetProcAddress as *mut c_void, name) }
}

/// # Safety
/// `name` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXGetProcAddressARB(name: *const c_char) -> *mut c_void {
    // SAFETY: the caller's contract.
    unsafe { get_proc_address(&GLX_PROC_ARB, glXGetProcAddressARB as *mut c_void, name) }
}

/// # Safety
/// `name` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglGetProcAddress(name: *const c_char) -> *mut c_void {
    // SAFETY: the caller's contract.
    unsafe { get_proc_address(&EGL_PROC, eglGetProcAddress as *mut c_void, name) }
}

static EXTENT: AtomicU64 = AtomicU64::new(0);

/// Paces the frame and draws the overlay over the current drawable of `size`, returning the
/// box it drew with a bottom-left origin.
fn present(game: &Game, size: Option<(u32, u32)>, gl: Option<&Gl>) -> Option<[i32; 4]> {
    if let Some((width, height)) = size {
        let packed = (u64::from(width) << 32) | u64::from(height);
        if EXTENT.swap(packed, Ordering::Relaxed) != packed {
            pacer::note_extent(width, height);
        }
    }
    pacer::before_present(game);
    let (_, height) = size?;
    let frame = pacer::overlay(height)?;
    // SAFETY: `size` is only known for the drawable current on this thread, so GL calls reach it.
    unsafe { gl?.draw(&frame, height) }
}

struct Glx {
    current_drawable: GlxGetCurrentDrawable,
    query_drawable: GlxQueryDrawable,
    gl: Option<Gl>,
}

static GLX: OnceLock<Option<Glx>> = OnceLock::new();

fn glx() -> Option<&'static Glx> {
    if let Some(glx) = GLX.get() {
        return glx.as_ref();
    }
    let gpa = real_proc_address(&GLX_PROC_ARB, glXGetProcAddressARB as *mut c_void, &GLX_SWAP)
        .or_else(|| real_proc_address(&GLX_PROC, glXGetProcAddress as *mut c_void, &GLX_SWAP))?;
    // SAFETY: NUL-terminated names to the driver's GetProcAddress.
    let lookup = |name: &CStr| unsafe { gpa(name.as_ptr()) };
    GLX.get_or_init(|| {
        Some(Glx {
            current_drawable: load!(lookup, c"glXGetCurrentDrawable", GlxGetCurrentDrawable),
            query_drawable: load!(lookup, c"glXQueryDrawable", GlxQueryDrawable),
            gl: Gl::load(lookup),
        })
    })
    .as_ref()
}

impl Glx {
    /// Only the current drawable: GL calls reach no other, and a query for a drawable Mesa has
    /// not made current goes to the X server, which can answer with an X error.
    unsafe fn current_size(&self, display: *mut c_void, drawable: c_ulong) -> Option<(u32, u32)> {
        // SAFETY: the game's own display and drawable, on the thread swapping it.
        unsafe {
            if drawable == 0 || (self.current_drawable)() != drawable {
                return None;
            }
            let (mut width, mut height) = (0, 0);
            (self.query_drawable)(display, drawable, GLX_WIDTH, &mut width);
            (self.query_drawable)(display, drawable, GLX_HEIGHT, &mut height);
            (width > 0 && height > 0).then_some((width, height))
        }
    }
}

/// # Safety
/// Called by the game with its own GLX display and drawable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn glXSwapBuffers(display: *mut c_void, drawable: c_ulong) {
    let real = GLX_SWAP.get(glXSwapBuffers as *mut c_void);
    if real.is_null() {
        return;
    }
    if let Some(game) = game() {
        // X11 window ids are 29 bits.
        #[allow(clippy::unnecessary_cast, reason = "c_ulong is u32 on x86")]
        pacer::note_x11_window(drawable as u32);
        let glx = glx();
        // SAFETY: the caller's display and drawable.
        let size = glx.and_then(|glx| unsafe { glx.current_size(display, drawable) });
        present(game, size, glx.and_then(|glx| glx.gl.as_ref()));
    }
    // SAFETY: forwarding the game's own call to the function found under this name.
    unsafe { std::mem::transmute::<*mut c_void, GlxSwapBuffers>(real)(display, drawable) }
}

struct Egl {
    current_surface: EglGetCurrentSurface,
    query_surface: EglQuerySurface,
    gl: Option<Gl>,
}

static EGL: OnceLock<Option<Egl>> = OnceLock::new();

fn egl() -> Option<&'static Egl> {
    if let Some(egl) = EGL.get() {
        return egl.as_ref();
    }
    let gpa = real_proc_address(&EGL_PROC, eglGetProcAddress as *mut c_void, &EGL_SWAP)?;
    // EGL before 1.5 need not hand out core functions through eglGetProcAddress.
    let lookup = |name: &CStr| {
        // SAFETY: NUL-terminated names to the driver's GetProcAddress.
        let f = unsafe { gpa(name.as_ptr()) };
        if f.is_null() { beside(EGL_SWAP.ptr.load(Ordering::Relaxed), name) } else { f }
    };
    EGL.get_or_init(|| {
        Some(Egl {
            current_surface: load!(lookup, c"eglGetCurrentSurface", EglGetCurrentSurface),
            query_surface: load!(lookup, c"eglQuerySurface", EglQuerySurface),
            gl: Gl::load(lookup),
        })
    })
    .as_ref()
}

impl Egl {
    unsafe fn current_size(&self, display: *mut c_void, surface: *mut c_void) -> Option<(u32, u32)> {
        // SAFETY: the game's own display and surface, on the thread swapping it.
        unsafe {
            if surface.is_null() || (self.current_surface)(EGL_DRAW) != surface {
                return None;
            }
            let (mut width, mut height) = (0, 0);
            let ok = (self.query_surface)(display, surface, EGL_WIDTH, &mut width) != 0
                && (self.query_surface)(display, surface, EGL_HEIGHT, &mut height) != 0;
            (ok && width > 0 && height > 0).then_some((width as u32, height as u32))
        }
    }
}

/// # Safety
/// The caller's own display and surface.
unsafe fn egl_present(display: *mut c_void, surface: *mut c_void) -> Option<[i32; 4]> {
    let game = game()?;
    let egl = egl();
    // SAFETY: the caller's contract.
    let size = egl.and_then(|egl| unsafe { egl.current_size(display, surface) });
    present(game, size, egl.and_then(|egl| egl.gl.as_ref()))
}

/// # Safety
/// Called by the game with its own EGL display and surface.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffers(display: *mut c_void, surface: *mut c_void) -> c_uint {
    let real = EGL_SWAP.get(eglSwapBuffers as *mut c_void);
    if real.is_null() {
        return 0;
    }
    // SAFETY: the caller's display and surface; then forwarding its call to the function found
    // under this name.
    unsafe {
        egl_present(display, surface);
        std::mem::transmute::<*mut c_void, EglSwapBuffers>(real)(display, surface)
    }
}

/// # Safety
/// The caller's own display, surface and `n` rectangles of four ints.
unsafe fn swap_with_damage(
    slot: &Real,
    ours: *mut c_void,
    display: *mut c_void,
    surface: *mut c_void,
    rects: *const i32,
    n: i32,
) -> c_uint {
    let real = slot.get(ours);
    if real.is_null() {
        return 0;
    }
    // SAFETY: the caller's contract; then forwarding its call to the function found under this
    // name. Damage outside the listed rectangles may never reach the screen, so the overlay's
    // box joins them. No rectangles means the whole surface.
    unsafe {
        let real = std::mem::transmute::<*mut c_void, EglSwapBuffersWithDamage>(real);
        match egl_present(display, surface) {
            Some(overlay) if n > 0 && !rects.is_null() => {
                let mut all = std::slice::from_raw_parts(rects, n as usize * 4).to_vec();
                all.extend_from_slice(&overlay);
                real(display, surface, all.as_ptr(), n + 1)
            }
            _ => real(display, surface, rects, n),
        }
    }
}

/// # Safety
/// Called by the game with its own EGL display, surface and damage rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffersWithDamageKHR(
    display: *mut c_void,
    surface: *mut c_void,
    rects: *const i32,
    n: i32,
) -> c_uint {
    let ours = eglSwapBuffersWithDamageKHR as *mut c_void;
    // SAFETY: the caller's contract.
    unsafe { swap_with_damage(&EGL_SWAP_DAMAGE_KHR, ours, display, surface, rects, n) }
}

/// # Safety
/// Called by the game with its own EGL display, surface and damage rectangles.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn eglSwapBuffersWithDamageEXT(
    display: *mut c_void,
    surface: *mut c_void,
    rects: *const i32,
    n: i32,
) -> c_uint {
    let ours = eglSwapBuffersWithDamageEXT as *mut c_void;
    // SAFETY: the caller's contract.
    unsafe { swap_with_damage(&EGL_SWAP_DAMAGE_EXT, ours, display, surface, rects, n) }
}
