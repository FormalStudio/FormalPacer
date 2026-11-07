//! The OpenGL overlay, shared by the Linux `LD_PRELOAD` hook and the Windows hook: scissored
//! `glClear`s into the default framebuffer. Every GL and GLES version
//! has them, and they leave no program, texture or buffer bound behind the game's back: only a
//! handful of state values, all put back before the swap.

use std::ffi::{CStr, c_void};
use std::sync::Mutex;

use crate::overlay::{BACKGROUND, COLORS, MARGIN};

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
use crate::pacer::Frame;

const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_SCISSOR_BOX: u32 = 0x0C10;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
const GL_COLOR_WRITEMASK: u32 = 0x0C23;
const GL_VERSION: u32 = 0x1F02;
/// `GL_FRAMEBUFFER_BINDING` on GLES 2, where reading and drawing share one binding.
const GL_DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_FRAMEBUFFER_SRGB: u32 = 0x8DB9;

type GetString = unsafe extern "system" fn(u32) -> *const u8;
type GetIntegerv = unsafe extern "system" fn(u32, *mut i32);
type GetFloatv = unsafe extern "system" fn(u32, *mut f32);
type GetBooleanv = unsafe extern "system" fn(u32, *mut u8);
type IsEnabled = unsafe extern "system" fn(u32) -> u8;
type Capability = unsafe extern "system" fn(u32);
type Scissor = unsafe extern "system" fn(i32, i32, i32, i32);
type ClearColor = unsafe extern "system" fn(f32, f32, f32, f32);
type ColorMask = unsafe extern "system" fn(u8, u8, u8, u8);
type Clear = unsafe extern "system" fn(u32);
type BindFramebuffer = unsafe extern "system" fn(u32, u32);

pub struct Gl {
    get_string: GetString,
    get_integerv: GetIntegerv,
    get_floatv: GetFloatv,
    get_booleanv: GetBooleanv,
    is_enabled: IsEnabled,
    enable: Capability,
    disable: Capability,
    scissor: Scissor,
    clear_color: ClearColor,
    color_mask: ColorMask,
    clear: Clear,
    bind_framebuffer: BindFramebuffer,
}

/// `(x, y, width, value)`, as [`crate::overlay::Bitmap::runs`] gives them.
type Run = (u32, u32, u32, u8);

/// The bitmap's runs, kept until its generation changes.
static RUNS: Mutex<(u64, Vec<Run>)> = Mutex::new((0, Vec::new()));

impl Gl {
    pub fn load(lookup: impl Fn(&CStr) -> *mut c_void) -> Option<Self> {
        Some(Self {
            get_string: load!(lookup, c"glGetString", GetString),
            get_integerv: load!(lookup, c"glGetIntegerv", GetIntegerv),
            get_floatv: load!(lookup, c"glGetFloatv", GetFloatv),
            get_booleanv: load!(lookup, c"glGetBooleanv", GetBooleanv),
            is_enabled: load!(lookup, c"glIsEnabled", IsEnabled),
            enable: load!(lookup, c"glEnable", Capability),
            disable: load!(lookup, c"glDisable", Capability),
            scissor: load!(lookup, c"glScissor", Scissor),
            clear_color: load!(lookup, c"glClearColor", ClearColor),
            color_mask: load!(lookup, c"glColorMask", ColorMask),
            clear: load!(lookup, c"glClear", Clear),
            bind_framebuffer: load!(lookup, c"glBindFramebuffer", BindFramebuffer),
        })
    }

    /// Whether the context is GLES and its major version. State the context's version does not
    /// have is left alone: querying it would leave a GL error for the game to find.
    unsafe fn version(&self) -> Option<(bool, u32)> {
        // SAFETY: a context is current; glGetString returns null or a NUL-terminated string.
        let version = unsafe {
            let v = (self.get_string)(GL_VERSION);
            if v.is_null() {
                return None;
            }
            CStr::from_ptr(v.cast()).to_bytes()
        };
        let (es, rest) = match version.strip_prefix(b"OpenGL ES") {
            Some(rest) => (true, rest),
            None => (false, version),
        };
        let major = rest
            .iter()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .fold(0, |n: u32, c| n.saturating_mul(10).saturating_add(u32::from(c - b'0')));
        Some((es, major))
    }

    /// Clears the overlay into the back buffer of the current drawable, `height` pixels tall,
    /// and returns its box with a bottom-left origin.
    ///
    /// # Safety
    /// A context is current on this thread and its drawable is the one being swapped.
    pub unsafe fn draw(&self, frame: &Frame, height: u32) -> Option<[i32; 4]> {
        let Ok(mut runs) = RUNS.lock() else { return None };
        if runs.0 != frame.generation {
            *runs = (frame.generation, frame.bitmap.runs());
        }
        let bitmap = &frame.bitmap;
        let cell = bitmap.scale as i32;
        let left = (MARGIN * bitmap.scale) as i32;
        // GL counts rows from the bottom.
        let bottom = |top: i32, rows: i32| height as i32 - left - top - rows;
        let area = [left, bottom(0, bitmap.height as i32), bitmap.width as i32, bitmap.height as i32];
        // SAFETY: the caller's contract; every value read here is written back below.
        unsafe {
            let (es, major) = self.version()?;
            let framebuffer = match (es, major) {
                (_, 3..) => Some(GL_DRAW_FRAMEBUFFER),
                (true, 2) => Some(GL_FRAMEBUFFER),
                _ => None,
            };
            let srgb = !es && major >= 3;

            let mut bound = 0;
            if framebuffer.is_some() {
                (self.get_integerv)(GL_DRAW_FRAMEBUFFER_BINDING, &mut bound);
            }
            let scissor_on = (self.is_enabled)(GL_SCISSOR_TEST);
            let mut scissor = [0; 4];
            (self.get_integerv)(GL_SCISSOR_BOX, scissor.as_mut_ptr());
            let mut clear = [0.0; 4];
            (self.get_floatv)(GL_COLOR_CLEAR_VALUE, clear.as_mut_ptr());
            let mut mask = [0; 4];
            (self.get_booleanv)(GL_COLOR_WRITEMASK, mask.as_mut_ptr());
            let srgb_on = srgb && (self.is_enabled)(GL_FRAMEBUFFER_SRGB) != 0;

            if let Some(target) = framebuffer
                && bound != 0
            {
                (self.bind_framebuffer)(target, 0);
            }
            (self.enable)(GL_SCISSOR_TEST);
            // An sRGB-capable back buffer would otherwise encode the clear colours a second time.
            if srgb_on {
                (self.disable)(GL_FRAMEBUFFER_SRGB);
            }
            (self.color_mask)(1, 1, 1, 1);

            self.fill(area, BACKGROUND);
            for value in (0..COLORS.len() as u8).filter(|&v| v != BACKGROUND) {
                self.set_color(value);
                for &(x, y, width, _) in runs.1.iter().filter(|r| r.3 == value) {
                    (self.scissor)(left + x as i32, bottom(y as i32, cell), width as i32, cell);
                    (self.clear)(GL_COLOR_BUFFER_BIT);
                }
            }

            (self.color_mask)(mask[0], mask[1], mask[2], mask[3]);
            if srgb_on {
                (self.enable)(GL_FRAMEBUFFER_SRGB);
            }
            (self.clear_color)(clear[0], clear[1], clear[2], clear[3]);
            (self.scissor)(scissor[0], scissor[1], scissor[2], scissor[3]);
            if scissor_on == 0 {
                (self.disable)(GL_SCISSOR_TEST);
            }
            if let Some(target) = framebuffer
                && bound != 0
            {
                (self.bind_framebuffer)(target, bound as u32);
            }
        }
        Some(area)
    }

    unsafe fn set_color(&self, value: u8) {
        let [r, g, b, a] = COLORS[usize::from(value)].map(|c| f32::from(c) / 255.0);
        // SAFETY: the caller has a context current.
        unsafe { (self.clear_color)(r, g, b, a) };
    }

    unsafe fn fill(&self, [x, y, width, height]: [i32; 4], value: u8) {
        // SAFETY: the caller has a context current with scissoring on.
        unsafe {
            self.set_color(value);
            (self.scissor)(x, y, width, height);
            (self.clear)(GL_COLOR_BUFFER_BIT);
        }
    }
}
