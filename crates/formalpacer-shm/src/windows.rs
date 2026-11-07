use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileSizeEx, OPEN_EXISTING,
};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READWRITE, UnmapViewOfFile,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

pub fn now_ns() -> u64 {
    static FREQ: OnceLock<u64> = OnceLock::new();
    let freq = *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: writes the counter's frequency, fixed at boot.
        unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1) as u64
    });
    let mut count = 0i64;
    // SAFETY: writes the counter.
    unsafe { QueryPerformanceCounter(&mut count) };
    (u128::from(count as u64) * 1_000_000_000 / u128::from(freq)) as u64
}

/// The user's temp directory, which other users cannot open.
pub fn default_dir() -> PathBuf {
    std::env::temp_dir().join("FormalPacer")
}

pub fn prefix() -> String {
    "formalpacer-".to_owned()
}

pub fn map(path: &Path, create: bool, size: usize) -> io::Result<NonNull<u8>> {
    if create && let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: Win32 calls on a NUL-terminated path; every handle is closed and every result checked.
    // The view keeps the mapping alive after its handles close.
    unsafe {
        let file = CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            if create { CREATE_NEW } else { OPEN_EXISTING },
            FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if file == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0i64;
        if !create && (GetFileSizeEx(file, &mut len) == 0 || (len as usize) < size) {
            CloseHandle(file);
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a page"));
        }
        let mapping = CreateFileMappingW(file, std::ptr::null(), PAGE_READWRITE, 0, size as u32, std::ptr::null());
        CloseHandle(file);
        if mapping.is_null() {
            return Err(io::Error::last_os_error());
        }
        let view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, size);
        CloseHandle(mapping);
        NonNull::new(view.Value.cast()).ok_or_else(io::Error::last_os_error)
    }
}

pub fn unmap(ptr: NonNull<u8>, _size: usize) {
    // SAFETY: unmaps exactly the view `map` returned.
    unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: ptr.as_ptr().cast() }) };
}
