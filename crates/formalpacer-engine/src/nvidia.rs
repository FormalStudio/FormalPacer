//! The NVIDIA driver, through NVAPI: whether G-SYNC is running on a display, and the driver's frame
//! rate limiter ("Max Frame Rate" in the NVIDIA app), set per executable through the driver
//! settings database (DRS) the way the NVIDIA app and Profile Inspector write it. Nothing is
//! loaded into the game. The driver reads a profile when the game starts its graphics, so a value
//! written here reaches the game on its next launch, not the running one.
//!
//! Function ids and struct layouts are from NVIDIA's public NVAPI headers
//! (github.com/NVIDIA/nvapi: `nvapi_interface.h`, `nvapi.h`, `NvApiDriverSettings.h`).

use std::ffi::c_void;
use std::io;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const FRL_FPS_ID: u32 = 0x1083_5002;
/// The limiter's largest value (`FRL_FPS_MAX`).
pub const MAX_FPS: u32 = 0x3ff;

const NVAPI_OK: i32 = 0;
const NVAPI_EXECUTABLE_NOT_FOUND: i32 = -166;
const NVAPI_SETTING_NOT_FOUND: i32 = -160;

/// Name of the profile FormalPacer creates for a game the driver has no profile for.
const OWN_PROFILE: &str = "FormalPacer";

const UNICODE_MAX: usize = 2048;
type UnicodeString = [u16; UNICODE_MAX];

const fn version<T>(v: u32) -> u32 {
    size_of::<T>() as u32 | (v << 16)
}

/// `NVDRS_SETTING_V1`, packed to 4 bytes. The value unions are 4100 bytes (a length and 4096 bytes
/// of binary data) and only their first `u32` is used here.
#[repr(C)]
struct Setting {
    version: u32,
    name: UnicodeString,
    id: u32,
    kind: u32,
    location: u32,
    is_current_predefined: u32,
    is_predefined_valid: u32,
    predefined: [u32; 1025],
    current: [u32; 1025],
}
const _: () = assert!(size_of::<Setting>() == 12320);

/// `NVDRS_PROFILE_V1`.
#[repr(C)]
struct Profile {
    version: u32,
    name: UnicodeString,
    gpu_support: u32,
    is_predefined: u32,
    apps: u32,
    settings: u32,
}

/// `NVDRS_APPLICATION_V4`.
#[repr(C)]
struct Application {
    version: u32,
    is_predefined: u32,
    name: UnicodeString,
    friendly_name: UnicodeString,
    launcher: UnicodeString,
    file_in_folder: UnicodeString,
    flags: u32,
    command_line: UnicodeString,
}

/// `NV_GET_VRR_INFO_V1`: a version and one word of flags, then reserved words.
#[repr(C)]
struct VrrInfo {
    version: u32,
    flags: u32,
    reserved: [u32; 4],
}

type Handle = *mut c_void;

type CreateSession = unsafe extern "C" fn(*mut Handle) -> i32;
type SessionCall = unsafe extern "C" fn(Handle) -> i32;
type FindApplication = unsafe extern "C" fn(Handle, *const u16, *mut Handle, *mut Application) -> i32;
type FindProfile = unsafe extern "C" fn(Handle, *const u16, *mut Handle) -> i32;
type CreateProfile = unsafe extern "C" fn(Handle, *mut Profile, *mut Handle) -> i32;
type CreateApplication = unsafe extern "C" fn(Handle, Handle, *mut Application) -> i32;
type GetSetting = unsafe extern "C" fn(Handle, Handle, u32, *mut Setting) -> i32;
type SetSetting = unsafe extern "C" fn(Handle, Handle, *mut Setting) -> i32;
type DeleteSetting = unsafe extern "C" fn(Handle, Handle, u32) -> i32;
type DisplayId = unsafe extern "C" fn(*const u8, *mut u32) -> i32;
type GetVrrInfo = unsafe extern "C" fn(u32, *mut VrrInfo) -> i32;

struct Api {
    create_session: CreateSession,
    destroy_session: SessionCall,
    load_settings: SessionCall,
    save_settings: SessionCall,
    find_application: FindApplication,
    find_profile: FindProfile,
    create_profile: CreateProfile,
    create_application: CreateApplication,
    get_setting: GetSetting,
    set_setting: SetSetting,
    delete_setting: DeleteSetting,
    display_id: DisplayId,
    vrr_info: GetVrrInfo,
}

fn api() -> Option<&'static Api> {
    static API: std::sync::OnceLock<Option<Api>> = std::sync::OnceLock::new();
    API.get_or_init(load).as_ref()
}

fn load() -> Option<Api> {
    let dll: Vec<u16> = "nvapi64.dll\0".encode_utf16().collect();
    // SAFETY: LoadLibraryW with a NUL-terminated name; nvapi_QueryInterface returns the function
    // for an id, and every id below is cast to the signature its header declares.
    unsafe {
        let lib = LoadLibraryW(dll.as_ptr());
        if lib.is_null() {
            return None;
        }
        let query = std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "C" fn(u32) -> *const c_void,
        >(GetProcAddress(lib, c"nvapi_QueryInterface".as_ptr().cast())?);
        let f = |id: u32| {
            let p = query(id);
            (!p.is_null()).then_some(p)
        };
        let initialize = std::mem::transmute::<*const c_void, unsafe extern "C" fn() -> i32>(f(0x0150_e828)?);
        if initialize() != NVAPI_OK {
            return None;
        }
        Some(Api {
            create_session: std::mem::transmute::<*const c_void, CreateSession>(f(0x0694_d52e)?),
            destroy_session: std::mem::transmute::<*const c_void, SessionCall>(f(0xdad9_cff8)?),
            load_settings: std::mem::transmute::<*const c_void, SessionCall>(f(0x375d_bd6b)?),
            save_settings: std::mem::transmute::<*const c_void, SessionCall>(f(0xfcbc_7e14)?),
            find_application: std::mem::transmute::<*const c_void, FindApplication>(f(0xeee5_66b2)?),
            find_profile: std::mem::transmute::<*const c_void, FindProfile>(f(0x7e4a_9a0b)?),
            create_profile: std::mem::transmute::<*const c_void, CreateProfile>(f(0xcc17_6068)?),
            create_application: std::mem::transmute::<*const c_void, CreateApplication>(f(0x4347_a9de)?),
            get_setting: std::mem::transmute::<*const c_void, GetSetting>(f(0x73bf_8338)?),
            set_setting: std::mem::transmute::<*const c_void, SetSetting>(f(0x577d_d202)?),
            delete_setting: std::mem::transmute::<*const c_void, DeleteSetting>(f(0xe4a2_6362)?),
            display_id: std::mem::transmute::<*const c_void, DisplayId>(f(0xae45_7190)?),
            vrr_info: std::mem::transmute::<*const c_void, GetVrrInfo>(f(0xdf8f_da57)?),
        })
    }
}

pub fn available() -> bool {
    api().is_some()
}

pub struct VrrState {
    /// G-SYNC is turned on for the display. It runs only while a game presents in a mode it
    /// covers, so the flag for that (`bIsVRREnabled`) is off whenever the desktop is in front.
    pub enabled: bool,
    /// The display and the driver can run it.
    pub possible: bool,
    /// It runs now: a game presents on its own in a mode it covers.
    pub running: bool,
}

/// G-SYNC on the display with GDI name `\\.\DISPLAY1`, or `None` when the NVIDIA driver does not
/// drive it.
pub fn vrr(gdi_name: &str) -> Option<VrrState> {
    let api = api()?;
    let name: Vec<u8> = gdi_name.bytes().chain([0]).collect();
    let mut id = 0u32;
    let mut info = VrrInfo { version: version::<VrrInfo>(1), flags: 0, reserved: [0; 4] };
    // SAFETY: a NUL-terminated name, then a versioned struct to fill.
    unsafe {
        if (api.display_id)(name.as_ptr(), &mut id) != NVAPI_OK || (api.vrr_info)(id, &mut info) != NVAPI_OK {
            return None;
        }
    }
    // Bit 0 is bIsVRREnabled (running now), bit 1 bIsVRRPossible, bit 4 bIsDisplayInVRRMode (turned
    // on for the display, whatever is in front).
    Some(VrrState { enabled: info.flags & 0x11 != 0, possible: info.flags & 2 != 0, running: info.flags & 1 != 0 })
}

fn wide(s: &str) -> Box<UnicodeString> {
    let mut out = Box::new([0u16; UNICODE_MAX]);
    for (slot, unit) in out.iter_mut().zip(s.encode_utf16().take(UNICODE_MAX - 1)) {
        *slot = unit;
    }
    out
}

/// Boxed and zeroed: the structs are tens of kilobytes, and every field is plain data.
fn zeroed<T>() -> Box<T> {
    // SAFETY: T is one of the repr(C) structs above, valid when all bytes are zero.
    unsafe { Box::new(std::mem::zeroed()) }
}

fn check(status: i32, what: &str) -> io::Result<()> {
    if status == NVAPI_OK { Ok(()) } else { Err(io::Error::other(format!("NVAPI {what} failed ({status})"))) }
}

struct Session<'a> {
    api: &'a Api,
    handle: Handle,
}

impl<'a> Session<'a> {
    fn open() -> io::Result<Self> {
        let api = api().ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no NVIDIA driver"))?;
        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY: NVAPI writes the session handle; it is destroyed on drop.
        check(unsafe { (api.create_session)(&mut handle) }, "create session")?;
        let session = Self { api, handle };
        // SAFETY: a live session.
        check(unsafe { (api.load_settings)(handle) }, "load settings")?;
        Ok(session)
    }

    /// The profile holding `exe`, if the driver has one.
    fn profile_of(&self, exe: &str) -> io::Result<Option<Handle>> {
        let name = wide(exe);
        let mut app = zeroed::<Application>();
        app.version = version::<Application>(4);
        let mut profile: Handle = std::ptr::null_mut();
        // SAFETY: a live session, a NUL-terminated name and a versioned struct to fill.
        match unsafe { (self.api.find_application)(self.handle, name.as_ptr(), &mut profile, &mut *app) } {
            NVAPI_OK => Ok(Some(profile)),
            NVAPI_EXECUTABLE_NOT_FOUND => Ok(None),
            status => check(status, "find application").map(|()| None),
        }
    }

    /// FormalPacer's own profile, created on first use, with `exe` added to it.
    fn own_profile_with(&self, exe: &str) -> io::Result<Handle> {
        let name = wide(OWN_PROFILE);
        let mut profile: Handle = std::ptr::null_mut();
        // SAFETY: a live session and a NUL-terminated name.
        if unsafe { (self.api.find_profile)(self.handle, name.as_ptr(), &mut profile) } != NVAPI_OK {
            let mut info = zeroed::<Profile>();
            info.version = version::<Profile>(1);
            info.name = *name;
            // SAFETY: a live session and a versioned struct.
            check(unsafe { (self.api.create_profile)(self.handle, &mut *info, &mut profile) }, "create profile")?;
        }
        let mut app = zeroed::<Application>();
        app.version = version::<Application>(4);
        app.name = *wide(exe);
        // SAFETY: a live session, a profile from it and a versioned struct.
        check(unsafe { (self.api.create_application)(self.handle, profile, &mut *app) }, "add application")?;
        Ok(profile)
    }

    fn cap(&self, profile: Handle) -> io::Result<Option<u32>> {
        let mut setting = zeroed::<Setting>();
        setting.version = version::<Setting>(1);
        // SAFETY: a live session, a profile from it and a versioned struct to fill.
        match unsafe { (self.api.get_setting)(self.handle, profile, FRL_FPS_ID, &mut *setting) } {
            // A value inherited from the global profile is not this game's.
            NVAPI_OK if setting.location == 0 => Ok(Some(setting.current[0])),
            NVAPI_OK | NVAPI_SETTING_NOT_FOUND => Ok(None),
            status => check(status, "read limiter").map(|()| None),
        }
    }

    fn save(&self) -> io::Result<()> {
        // SAFETY: a live session.
        check(unsafe { (self.api.save_settings)(self.handle) }, "save settings")
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        // SAFETY: destroys the session `open` created, once.
        unsafe { (self.api.destroy_session)(self.handle) };
    }
}

/// The limiter value in `exe`'s own profile, if one is set there.
pub fn cap(exe: &str) -> io::Result<Option<u32>> {
    let session = Session::open()?;
    match session.profile_of(exe)? {
        Some(profile) => session.cap(profile),
        None => Ok(None),
    }
}

/// Sets `exe`'s limiter to `fps`, or removes it with `None`.
pub fn set_cap(exe: &str, fps: Option<u32>) -> io::Result<()> {
    let session = Session::open()?;
    let profile = match (session.profile_of(exe)?, fps) {
        (Some(profile), _) => profile,
        (None, None) => return Ok(()),
        (None, Some(_)) => session.own_profile_with(exe)?,
    };
    match fps {
        Some(fps) => {
            let mut setting = zeroed::<Setting>();
            setting.version = version::<Setting>(1);
            setting.id = FRL_FPS_ID;
            setting.current[0] = fps.min(MAX_FPS);
            // SAFETY: a live session, a profile from it and a versioned DWORD setting.
            check(unsafe { (session.api.set_setting)(session.handle, profile, &mut *setting) }, "set limiter")?;
        }
        None => {
            // SAFETY: a live session and a profile from it.
            let status = unsafe { (session.api.delete_setting)(session.handle, profile, FRL_FPS_ID) };
            if status != NVAPI_SETTING_NOT_FOUND {
                check(status, "clear limiter")?;
            }
        }
    }
    session.save()
}
