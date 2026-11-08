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
//! RTSS waits inside the game's present, so PresentMon counts the wait as the frame's work: under
//! an RTSS cap a game looks exactly as fast as the cap (see `Pacer::update`'s `blind`).

use std::collections::HashSet;
use std::ffi::{CString, c_char, c_void};
use std::io;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, VirtualQuery,
};

use crate::config::{Config, RtssProfile};
use crate::engine::Rtss;

const KEY: &str = "SOFTWARE\\WOW6432Node\\Unwinder\\RTSS";
const SHARED_MEMORY: &str = "RTSSSharedMemoryV2";
/// The header's `dwSignature`, `'RTSS'` as MSVC packs it; it turns to `0xDEAD` once RTSS exits.
const SIGNATURE: u32 = u32::from_be_bytes(*b"RTSS");
const LIMIT: &std::ffi::CStr = c"FramerateLimit";

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

/// The processes RTSS is in, or `None` when RTSS is not running.
pub fn hooked() -> Option<HashSet<u32>> {
    let name = wide(SHARED_MEMORY);
    // SAFETY: a NUL-terminated name; the handle is closed below.
    let mapping = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
    if mapping.is_null() {
        return None;
    }
    // SAFETY: a read-only view of the whole mapping, unmapped below.
    let view = unsafe { MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0) };
    let pids = (!view.Value.is_null()).then(|| {
        // SAFETY: a live view, read only within the region VirtualQuery reports.
        unsafe { pids(view.Value.cast::<u8>()) }
    });
    // SAFETY: the view and handle from above, released once.
    unsafe {
        if !view.Value.is_null() {
            UnmapViewOfFile(view);
        }
        CloseHandle(mapping);
    }
    pids.flatten()
}

/// The `dwProcessID` of every application entry, after the header RTSSSharedMemory.h lays out:
/// signature, version, entry size, entry array offset, entry count.
///
/// # Safety
/// `base` is the start of a mapped view.
unsafe fn pids(base: *const u8) -> Option<HashSet<u32>> {
    // SAFETY: plain data, valid when zeroed.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a pointer into this process's address space and a sized struct to fill.
    if unsafe { VirtualQuery(base.cast(), &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) } == 0 {
        return None;
    }
    let size = info.RegionSize;
    // SAFETY: only offsets inside the mapped region are read.
    let word = |at: usize| (at + 4 <= size).then(|| unsafe { base.add(at).cast::<u32>().read_unaligned() });
    if word(0)? != SIGNATURE || word(4)? < 0x0002_0000 {
        return None;
    }
    let (entry, offset, count) = (word(8)? as usize, word(12)? as usize, word(16)? as usize);
    Some((0..count).filter_map(|i| word(offset + i * entry)).filter(|&pid| pid != 0).collect())
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
