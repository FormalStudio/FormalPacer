//! The few registry reads and writes FormalPacer needs on Windows.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegEnumValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};

const IMPLICIT_LAYERS: &str = "SOFTWARE\\Khronos\\Vulkan\\ImplicitLayers";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// The manifests registered as implicit layers, for this user and for the machine.
pub fn implicit_layers() -> Vec<PathBuf> {
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .flat_map(|root| value_names(root, IMPLICIT_LAYERS))
        .map(PathBuf::from)
        .collect()
}

fn value_names(root: HKEY, subkey: &str) -> Vec<String> {
    let subkey = wide(subkey);
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: a NUL-terminated subkey; the opened key is closed below.
    if unsafe { RegOpenKeyExW(root, subkey.as_ptr(), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut names = Vec::new();
    for i in 0.. {
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        // SAFETY: a buffer of the length passed; only the name is read.
        let status = unsafe {
            RegEnumValueW(
                key,
                i,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    // SAFETY: the key opened above.
    unsafe { RegCloseKey(key) };
    names
}

/// Registers `manifest` as an enabled implicit layer for this user: a DWORD 0 named by its path.
pub fn register_implicit_layer(manifest: &Path) -> io::Result<()> {
    let subkey = wide(IMPLICIT_LAYERS);
    let name: Vec<u16> = manifest.as_os_str().encode_wide().chain([0]).collect();
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: NUL-terminated names; the created key is closed below.
    unsafe {
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let zero = 0u32.to_le_bytes();
        let status = RegSetValueExW(key, name.as_ptr(), 0, REG_DWORD, zero.as_ptr(), zero.len() as u32);
        RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
    }
    Ok(())
}

/// A string value under `HKEY_CURRENT_USER`.
pub fn user_string(subkey: &str, value: &str) -> Option<String> {
    let (subkey, value) = (wide(subkey), wide(value));
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: NUL-terminated names and a buffer of the byte length passed.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    (status == ERROR_SUCCESS).then(|| {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    })
}
