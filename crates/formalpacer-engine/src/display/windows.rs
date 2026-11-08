//! Every active display from the Windows display configuration: the GDI name (`DISPLAY1`) is the
//! connector, the target's refresh rate is exact (a rational, 143.998 Hz rather than 144), the
//! EDID is the one the monitor's device node keeps in the registry, and the NVIDIA driver says
//! whether G-SYNC is running on it.

use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Graphics::Gdi::{DEVMODEW, ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY, RegGetValueW};

use super::{Display, Vrr, edid_name, edid_vrr_range};
use crate::nvidia;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

pub fn probe() -> Vec<Display> {
    let (paths, modes) = active_paths();
    // The driver can report G-SYNC on another of its displays than the one it runs on: after a
    // reboot renumbered the displays, the flags followed the number to a portable panel while the
    // one G-SYNC ran on read all clear. G-SYNC on for any display, then, is on for every one
    // whose EDID has a VRR window.
    let gsync_on = paths.iter().filter_map(source_name).filter_map(|s| nvidia::vrr(&s)).any(|v| v.enabled);
    paths
        .iter()
        .filter_map(|path| {
            let source = source_name(path)?;
            let target = target_name(path);
            // SAFETY: modeInfoIdx is the plain index variant of the source info union.
            let mode = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
            let mode = modes.get(mode).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)?;
            // SAFETY: a source mode, as its type says.
            let size = unsafe { mode.Anonymous.sourceMode };
            let rate = path.targetInfo.refreshRate;
            let refresh_hz =
                if rate.Denominator == 0 { 0. } else { f64::from(rate.Numerator) / f64::from(rate.Denominator) };
            let edid = target.as_ref().map(|t| edid(&t.1)).unwrap_or_default();
            let vrr_range = edid_vrr_range(&edid);
            let vrr = match nvidia::vrr(&source) {
                Some(state) if state.enabled => Vrr::Active,
                Some(_) if gsync_on && vrr_range.is_some() => Vrr::Active,
                Some(state) if state.possible => Vrr::Off,
                Some(_) => Vrr::Unsupported,
                // Another vendor's driver, or a panel the NVIDIA GPU does not drive: the EDID says
                // whether the panel can, nothing says whether it is on.
                None if vrr_range.is_some() => Vrr::Off,
                None => Vrr::Unsupported,
            };
            Some(Display {
                connector: source.trim_start_matches("\\\\.\\").to_owned(),
                model: edid_name(&edid).or_else(|| target.map(|t| t.0).filter(|n| !n.is_empty())),
                width: size.width as u16,
                height: size.height as u16,
                max_refresh_hz: max_refresh(&source, size.width, size.height).max(refresh_hz),
                refresh_hz,
                vrr,
                vrr_range,
            })
        })
        .collect()
}

fn active_paths() -> (Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>) {
    // The configuration can change between asking for the sizes and reading it; retry then.
    for _ in 0..3 {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        // SAFETY: writes the two counts.
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes) } != ERROR_SUCCESS {
            return (Vec::new(), Vec::new());
        }
        // SAFETY: plain data structs, valid when zeroed.
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![unsafe { std::mem::zeroed() }; n_paths as usize];
        // SAFETY: as above.
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![unsafe { std::mem::zeroed() }; n_modes as usize];
        // SAFETY: buffers of the sizes passed; the counts come back as what was written.
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut n_paths,
                paths.as_mut_ptr(),
                &mut n_modes,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if status == ERROR_SUCCESS {
            paths.truncate(n_paths as usize);
            modes.truncate(n_modes as usize);
            return (paths, modes);
        }
    }
    (Vec::new(), Vec::new())
}

/// `\\.\DISPLAY1`.
fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    // SAFETY: plain data, valid when zeroed.
    let mut name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = unsafe { std::mem::zeroed() };
    name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
        adapterId: path.sourceInfo.adapterId,
        id: path.sourceInfo.id,
    };
    // SAFETY: a header naming the struct it heads and its size.
    (unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } == 0).then(|| from_wide(&name.viewGdiDeviceName))
}

/// The monitor's friendly name and its device path.
fn target_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<(String, String)> {
    // SAFETY: plain data, valid when zeroed.
    let mut name: DISPLAYCONFIG_TARGET_DEVICE_NAME = unsafe { std::mem::zeroed() };
    name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
        adapterId: path.targetInfo.adapterId,
        id: path.targetInfo.id,
    };
    // SAFETY: a header naming the struct it heads and its size.
    (unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } == 0)
        .then(|| (from_wide(&name.monitorFriendlyDeviceName), from_wide(&name.monitorDevicePath)))
}

/// The EDID of the monitor at `\\?\DISPLAY#BOE0B66#4&2b5c&0&UID8388688#{guid}`, from
/// `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\BOE0B66\4&2b5c&0&UID8388688\Device Parameters`.
fn edid(device_path: &str) -> Vec<u8> {
    let parts: Vec<&str> = device_path.split('#').collect();
    let [_, model, instance, ..] = parts.as_slice() else { return Vec::new() };
    let key = wide(&format!("SYSTEM\\CurrentControlSet\\Enum\\DISPLAY\\{model}\\{instance}\\Device Parameters"));
    let value = wide("EDID");
    let mut buf = vec![0u8; 512];
    let mut len = buf.len() as u32;
    // SAFETY: NUL-terminated names and a buffer of the length passed.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if status != ERROR_SUCCESS {
        return Vec::new();
    }
    buf.truncate(len as usize);
    buf
}

/// The fastest mode the display offers at `width` x `height`.
fn max_refresh(gdi_name: &str, width: u32, height: u32) -> f64 {
    let name = wide(gdi_name);
    let mut best = 0u32;
    for i in 0.. {
        // SAFETY: plain data, valid when zeroed.
        let mut mode: DEVMODEW = unsafe { std::mem::zeroed() };
        mode.dmSize = size_of::<DEVMODEW>() as u16;
        // SAFETY: a NUL-terminated device name and a sized DEVMODEW to fill.
        if unsafe { EnumDisplaySettingsW(name.as_ptr(), i as ENUM_DISPLAY_SETTINGS_MODE, &mut mode) } == 0 {
            break;
        }
        if mode.dmPelsWidth == width && mode.dmPelsHeight == height {
            best = best.max(mode.dmDisplayFrequency);
        }
    }
    f64::from(best)
}
