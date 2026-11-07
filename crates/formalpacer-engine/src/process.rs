//! Other processes, seen from outside: whether one is still running and, on Windows, where its
//! executable is. On Windows only `PROCESS_QUERY_LIMITED_INFORMATION` is asked for, which Task
//! Manager has for every process, protected ones included.

#[cfg(windows)]
use std::path::PathBuf;

#[cfg(not(windows))]
pub fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
fn open(pid: u32) -> Option<windows_sys::Win32::Foundation::HANDLE> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: opens a handle the caller closes.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    (!handle.is_null()).then_some(handle)
}

#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    let Some(handle) = open(pid) else { return false };
    let mut code = 0u32;
    // SAFETY: a live handle, closed right after.
    let running = unsafe { GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32 };
    // SAFETY: as above.
    unsafe { CloseHandle(handle) };
    running
}

#[cfg(windows)]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{PROCESS_NAME_WIN32, QueryFullProcessImageNameW};
    let handle = open(pid)?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: a live handle and a buffer of the length passed; the handle is closed right after.
    let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0 };
    // SAFETY: as above.
    unsafe { CloseHandle(handle) };
    ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}
