//! RivaTuner Statistics Server (RTSS), when it is installed and running. Its frame rate limiter
//! changes a running game's rate from the next frame, where the NVIDIA driver's waits for the
//! game's next launch. RTSS loads itself into games, anti-cheat ones included, which let it in as
//! a signed overlay they have long known; nothing of FormalPacer's goes in. FormalPacer sets the
//! limit in the game's RTSS profile through RTSS's own `RTSSHooks64.dll`, and reads RTSS's shared
//! memory for the games RTSS is in.
//!
//! The profiles are files in RTSS's folder under Program Files, which only administrators can
//! write until FormalPacer is let in once (`elevate::allow_rtss_profiles`). A profile FormalPacer
//! made is deleted again when the game exits or pacing is switched off; one that was there before
//! gets its own limit back.
//!
//! The overlay goes into RTSS's on-screen display too, for the games RTSS is in: drawn inside the
//! game's own frames it leaves the game's present alone, where a window over the game makes
//! Windows compose it and turns G-SYNC off.
//!
//! RTSS waits inside the game's present, so PresentMon counts the wait as the frame's work: under
//! an RTSS cap a game looks exactly as fast as the cap (see `Pacer::update`'s `blind`).

use std::collections::HashSet;
use std::ffi::{CString, c_char, c_void};
use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_BASIC_INFORMATION, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile,
    VirtualQuery,
};

use crate::config::{Config, RtssProfile};
use crate::engine::Rtss;

const KEY: &str = "SOFTWARE\\WOW6432Node\\Unwinder\\RTSS";
const SHARED_MEMORY: &str = "RTSSSharedMemoryV2";
/// The header's `dwSignature`, `'RTSS'` as MSVC packs it; it turns to `0xDEAD` once RTSS exits.
const SIGNATURE: u32 = u32::from_be_bytes(*b"RTSS");
const LIMIT: &std::ffi::CStr = c"FramerateLimit";
/// The owner FormalPacer's OSD slot is claimed under.
const OSD_OWNER: &[u8] = b"FormalPacer";
/// The shared memory version with the OSD lock, `dwBusy` (2.14).
const OSD_LOCKED_VERSION: u32 = 0x0002_000e;
/// Offsets in RTSS_SHARED_MEMORY_OSD_ENTRY: `szOSDOwner[256]`, `szOSDEx[4096]`, then `buffer`,
/// which holds the objects the text embeds.
const OSD_OWNER_AT: usize = 256;
const OSD_TEXT_AT: usize = 512;
const OSD_TEXT_LEN: usize = 4096;
const OSD_BUFFER_AT: usize = OSD_TEXT_AT + OSD_TEXT_LEN;
/// RTSS_EMBEDDED_OBJECT_GRAPH: the object header (signature, size, width, height, margin), then
/// flags, min, max and the data count, with no data when RTSS plots the frame times itself.
const GRAPH_SIZE: usize = 36;
const GRAPH_SIGNATURE: u32 = u32::from_be_bytes(*b"GR00");
/// RTSS_EMBEDDED_OBJECT_GRAPH_FLAG_FRAMETIME: RTSS fills the graph with the game's frame times,
/// in microseconds, every frame. RTSS_EMBEDDED_OBJECT_GRAPH_FLAG_BGND: on a box, which shows
/// where the middle is.
const GRAPH_FLAGS: u32 = 4 | 16;
/// The graph's size; negative sizes are in characters and lines of the OSD's font.
const GRAPH_WIDTH: i32 = -20;
const GRAPH_HEIGHT: i32 = -3;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn dir() -> Option<PathBuf> {
    crate::registry::machine_string(KEY, "InstallDir")
        .map(PathBuf::from)
        .filter(|d| d.join("RTSSHooks64.dll").is_file())
}

pub fn profiles() -> Option<PathBuf> {
    dir().map(|d| d.join("Profiles"))
}

pub fn state() -> Rtss {
    let Some(profiles) = profiles() else { return Rtss::Missing };
    if hooked().is_none() {
        return Rtss::Stopped;
    }
    let probe = profiles.join(".formalpacer");
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Rtss::Ready
        }
        Err(_) => Rtss::Locked,
    }
}

/// Runs `f` on a view of RTSS's shared memory and its size, or returns `None` when RTSS is not
/// running.
fn with_shared<T>(access: u32, f: impl FnOnce(*mut u8, usize) -> Option<T>) -> Option<T> {
    let name = wide(SHARED_MEMORY);
    // SAFETY: a NUL-terminated name; the handle is closed below.
    let mapping = unsafe { OpenFileMappingW(access, 0, name.as_ptr()) };
    if mapping.is_null() {
        return None;
    }
    // SAFETY: a view of the whole mapping, unmapped below.
    let view = unsafe { MapViewOfFile(mapping, access, 0, 0, 0) };
    let base = view.Value.cast::<u8>();
    // SAFETY: plain data, valid when zeroed.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let size = (!base.is_null()
        // SAFETY: a pointer into this process's address space and a sized struct to fill.
        && unsafe { VirtualQuery(base.cast(), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } != 0)
        .then_some(info.RegionSize);
    let result = size.and_then(|size| f(base, size));
    // SAFETY: the view and handle from above, released once.
    unsafe {
        if !base.is_null() {
            UnmapViewOfFile(view);
        }
        CloseHandle(mapping);
    }
    result
}

/// Reads the `u32` at `at` in a view of `size` bytes, after checking it is inside.
///
/// # Safety
/// `base` is the start of a live view of at least `size` bytes.
unsafe fn word(base: *const u8, size: usize, at: usize) -> Option<u32> {
    // SAFETY: inside the view, as checked.
    (at + 4 <= size).then(|| unsafe { base.add(at).cast::<u32>().read_unaligned() })
}

/// The processes RTSS is in, or `None` when RTSS is not running: the `dwProcessID` of every
/// application entry, after the header RTSSSharedMemory.h lays out (signature, version, entry
/// size, entry array offset, entry count).
pub fn hooked() -> Option<HashSet<u32>> {
    with_shared(FILE_MAP_READ, |base, size| {
        // SAFETY: a live view of `size` bytes, read only through `word`.
        let word = |at| unsafe { word(base, size, at) };
        if word(0)? != SIGNATURE || word(4)? < 0x0002_0000 {
            return None;
        }
        let (entry, offset, count) = (word(8)? as usize, word(12)? as usize, word(16)? as usize);
        Some((0..count).filter_map(|i| word(offset + i * entry)).filter(|&pid| pid != 0).collect())
    })
}

/// FormalPacer's lines in RTSS's on-screen display, in the games RTSS is in.
pub struct Osd {
    shown: bool,
}

impl Osd {
    /// Takes down the lines a FormalPacer that did not exit cleanly left up.
    pub fn new() -> Self {
        write_osd(None);
        Self { shown: false }
    }

    /// Shows `lines` and a graph of the game's frame times, with the frame interval
    /// `interval_us` in its middle so a game paced evenly draws a flat line there.
    pub fn show(&mut self, text: Option<(&str, f32)>) {
        match text {
            Some(text) => self.shown |= write_osd(Some(text)),
            None if self.shown => self.shown = !write_osd(None),
            None => {}
        }
    }
}

impl Drop for Osd {
    fn drop(&mut self) {
        self.show(None);
    }
}

/// Writes the lines and the frame time graph into FormalPacer's OSD slot, claiming a free one
/// first, or empties the slot for `None`, the way RTSS's SDK sample does. Slot 0 is left to Afterburner and its kind. Returns
/// whether it was done: RTSS may hold the OSD while it draws, and then the next tick tries again.
fn write_osd(text: Option<(&str, f32)>) -> bool {
    with_shared(FILE_MAP_READ | FILE_MAP_WRITE, |base, size| {
        // SAFETY: a live view of `size` bytes; every offset below is checked against it.
        unsafe {
            let word = |at| word(base, size, at);
            if word(0)? != SIGNATURE || word(4)? < OSD_LOCKED_VERSION {
                return None;
            }
            let (entry, offset, count) = (word(20)? as usize, word(24)? as usize, word(28)? as usize);
            if entry < OSD_BUFFER_AT + GRAPH_SIZE || offset + count * entry > size {
                return None;
            }
            let slot = |i: usize| base.add(offset + i * entry);
            let owner = |i: usize| {
                let field = std::slice::from_raw_parts(slot(i).add(OSD_OWNER_AT), OSD_TEXT_AT - OSD_OWNER_AT);
                field.split(|&c| c == 0).next().unwrap_or(field)
            };
            let frame = AtomicU32::from_ptr(base.add(32).cast());
            let owned = (1..count).find(|&i| owner(i) == OSD_OWNER);
            let Some((lines, interval_us)) = text else {
                if let Some(i) = owned {
                    slot(i).write_bytes(0, entry);
                    frame.fetch_add(1, Ordering::SeqCst);
                }
                return Some(());
            };
            let i = owned.or_else(|| (1..count).find(|&i| owner(i).is_empty()))?;
            let busy = AtomicI32::from_ptr(base.add(36).cast());
            if busy.fetch_or(1, Ordering::SeqCst) & 1 != 0 {
                return None;
            }
            let owner_field = slot(i).add(OSD_OWNER_AT);
            owner_field.copy_from_nonoverlapping(OSD_OWNER.as_ptr(), OSD_OWNER.len());
            owner_field.add(OSD_OWNER.len()).write(0);
            let graph: [[u8; 4]; GRAPH_SIZE / 4] = [
                GRAPH_SIGNATURE.to_le_bytes(),
                (GRAPH_SIZE as u32).to_le_bytes(),
                GRAPH_WIDTH.to_le_bytes(),
                GRAPH_HEIGHT.to_le_bytes(),
                1i32.to_le_bytes(),
                GRAPH_FLAGS.to_le_bytes(),
                0f32.to_le_bytes(),
                (2. * interval_us).to_le_bytes(),
                0u32.to_le_bytes(),
            ];
            slot(i).add(OSD_BUFFER_AT).copy_from_nonoverlapping(graph.as_flattened().as_ptr(), GRAPH_SIZE);
            let text = format!("{lines}\n<OBJ=00000000>");
            let len = text.len().min(OSD_TEXT_LEN - 1);
            let text_field = slot(i).add(OSD_TEXT_AT);
            text_field.copy_from_nonoverlapping(text.as_ptr(), len);
            text_field.add(len).write(0);
            busy.fetch_and(!1, Ordering::SeqCst);
            frame.fetch_add(1, Ordering::SeqCst);
            Some(())
        }
    })
    .is_some()
}

type ProfileCall = unsafe extern "C" fn(*const c_char);
type PropertyCall = unsafe extern "C" fn(*const c_char, *mut c_void, u32) -> i32;

/// The profile calls RTSSHooks64.dll exports, as RTSS's own tools call them.
struct Hooks {
    load_profile: ProfileCall,
    save_profile: ProfileCall,
    delete_profile: ProfileCall,
    get_property: PropertyCall,
    set_property: PropertyCall,
    update_profiles: unsafe extern "C" fn(),
}

fn hooks() -> io::Result<&'static Hooks> {
    static HOOKS: OnceLock<Option<Hooks>> = OnceLock::new();
    HOOKS
        .get_or_init(|| {
            let path = dir()?.join("RTSSHooks64.dll");
            let path = wide(&path.to_string_lossy());
            // SAFETY: a NUL-terminated path. The library stays loaded for the process's life.
            let module = unsafe { LoadLibraryW(path.as_ptr()) };
            if module.is_null() {
                return None;
            }
            // SAFETY: each export has the signature RTSS's tools call it with.
            unsafe {
                let f =
                    |name: &std::ffi::CStr| GetProcAddress(module, name.as_ptr().cast()).map(|p| p as *const c_void);
                Some(Hooks {
                    load_profile: std::mem::transmute::<*const c_void, ProfileCall>(f(c"LoadProfile")?),
                    save_profile: std::mem::transmute::<*const c_void, ProfileCall>(f(c"SaveProfile")?),
                    delete_profile: std::mem::transmute::<*const c_void, ProfileCall>(f(c"DeleteProfile")?),
                    get_property: std::mem::transmute::<*const c_void, PropertyCall>(f(c"GetProfileProperty")?),
                    set_property: std::mem::transmute::<*const c_void, PropertyCall>(f(c"SetProfileProperty")?),
                    update_profiles: std::mem::transmute::<*const c_void, unsafe extern "C" fn()>(f(
                        c"UpdateProfiles",
                    )?),
                })
            }
        })
        .as_ref()
        .ok_or_else(|| io::Error::other("RTSSHooks64.dll did not load"))
}

fn profile_name(exe: &str) -> io::Result<CString> {
    CString::new(exe).map_err(io::Error::other)
}

fn limit_of(exe: &str) -> io::Result<u32> {
    let (hooks, name) = (hooks()?, profile_name(exe)?);
    let mut limit = 0u32;
    // SAFETY: a NUL-terminated profile name, and a DWORD of the size passed.
    unsafe {
        (hooks.load_profile)(name.as_ptr());
        (hooks.get_property)(LIMIT.as_ptr(), (&raw mut limit).cast(), 4);
    }
    Ok(limit)
}

/// Sets `exe`'s limit, 0 for none, and has RTSS apply it to the running game.
fn set(exe: &str, fps: u32) -> io::Result<()> {
    let (hooks, name) = (hooks()?, profile_name(exe)?);
    let mut value = fps;
    // SAFETY: a NUL-terminated profile name, and a DWORD of the size passed.
    unsafe {
        (hooks.load_profile)(name.as_ptr());
        if (hooks.set_property)(LIMIT.as_ptr(), (&raw mut value).cast(), 4) == 0 {
            return Err(io::Error::other("RTSS refused the limit"));
        }
        (hooks.save_profile)(name.as_ptr());
        (hooks.update_profiles)();
    }
    // SaveProfile says nothing when it cannot write the file.
    if limit_of(exe)? != fps {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "RTSS's profiles are not writable"));
    }
    Ok(())
}

fn restore(exe: &str, profile: RtssProfile) -> io::Result<()> {
    if profile.existed {
        return set(exe, profile.limit);
    }
    let (hooks, name) = (hooks()?, profile_name(exe)?);
    // SAFETY: a NUL-terminated profile name.
    unsafe {
        (hooks.delete_profile)(name.as_ptr());
        (hooks.update_profiles)();
    }
    Ok(())
}

/// Gives `exe` its own RTSS profile back, if FormalPacer changed it.
pub fn release(exe: &str, config: &mut Config) -> io::Result<()> {
    let Some(profile) = config.rtss_profiles.get(exe).copied() else { return Ok(()) };
    restore(exe, profile)?;
    config.rtss_profiles.remove(exe);
    crate::config::save(config)
}

/// Gives every game its own RTSS profile back.
pub fn restore_all(config: &mut Config) -> io::Result<()> {
    let exes: Vec<String> = config.rtss_profiles.keys().cloned().collect();
    exes.iter().try_for_each(|exe| release(exe, config))
}

/// One running game's RTSS limit.
pub struct Limit {
    exe: String,
    applied: Option<u32>,
}

impl Limit {
    pub fn new(exe: &str) -> Self {
        Self { exe: exe.to_owned(), applied: None }
    }

    pub fn exe(&self) -> &str {
        &self.exe
    }

    /// The limit RTSS holds the game at now.
    pub fn applied(&self) -> Option<u32> {
        self.applied
    }

    /// The planner's cap for this game now; RTSS applies it from the next frame.
    pub fn want(&mut self, fps: Option<f64>, config: &mut Config) -> io::Result<()> {
        let fps = fps.map(|f| f.round() as u32).filter(|&f| f > 0);
        if fps == self.applied {
            return Ok(());
        }
        if !config.rtss_profiles.contains_key(&self.exe) {
            let existed = profiles().is_some_and(|p| p.join(format!("{}.cfg", self.exe)).is_file());
            let limit = if existed { limit_of(&self.exe)? } else { 0 };
            config.rtss_profiles.insert(self.exe.clone(), RtssProfile { existed, limit });
            crate::config::save(config)?;
        }
        set(&self.exe, fps.unwrap_or(0))?;
        self.applied = fps;
        Ok(())
    }

    /// After [`restore_all`] or [`release`]: the game has its own limit again.
    pub fn released(&mut self) {
        self.applied = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run with RTSS running, `FORMALPACER_RTSS_PID` naming a game RTSS is in and `APPDATA` pointed
    /// somewhere disposable: the config is saved there.
    #[test]
    #[ignore = "needs RTSS and a game it is in"]
    fn caps_a_running_game_and_gives_its_profile_back() {
        let pid: u32 = std::env::var("FORMALPACER_RTSS_PID").unwrap().parse().unwrap();
        let exe = std::env::var("FORMALPACER_RTSS_EXE").unwrap();
        assert_eq!(state(), Rtss::Ready);
        assert!(hooked().unwrap().contains(&pid));
        let file = profiles().unwrap().join(format!("{exe}.cfg"));
        let had = file.is_file();
        let mut config = Config::default();
        let mut limit = Limit::new(&exe);
        for fps in [60., 100.] {
            limit.want(Some(fps), &mut config).unwrap();
            assert_eq!(limit_of(&exe).unwrap(), fps as u32);
            eprintln!("rtss: {fps} at {:?}", std::time::SystemTime::now());
            std::thread::sleep(std::time::Duration::from_secs(4));
        }
        release(&exe, &mut config).unwrap();
        assert!(config.rtss_profiles.is_empty());
        assert_eq!(file.is_file(), had);
    }
}
