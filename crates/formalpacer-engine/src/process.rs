//! Other processes, seen from outside: whether one is still running and, on Windows, where its
//! executable is. Those two ask only for `PROCESS_QUERY_LIMITED_INFORMATION`, which Task Manager
//! has for every process, protected ones included. On Windows FormalPacer also loads its hook
//! into games without an anti-cheat.

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

/// Loads `dll` into the 64-bit process `pid` with `LoadLibraryW` on a thread of its own, and waits
/// up to five seconds for it to load. The process must be this user's and not elevated above it.
#[cfg(windows)]
pub fn inject(pid: u32, dll: &std::path::Path) -> std::io::Result<()> {
    use std::io::Error;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
    };
    use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
    use windows_sys::Win32::System::Threading::{
        CreateRemoteThread, IsWow64Process2, OpenProcess, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION,
        PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE, WaitForSingleObject,
    };

    type Proc = unsafe extern "system" fn() -> isize;
    type ThreadStart = unsafe extern "system" fn(*mut std::ffi::c_void) -> u32;

    let path: Vec<u16> = dll.as_os_str().encode_wide().chain([0]).collect();
    let bytes = path.len() * 2;
    // What CreateRemoteThread documents it needs.
    let access =
        PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_READ | PROCESS_VM_WRITE;
    // SAFETY: every handle and allocation made here is released before returning; the remote
    // thread starts at `LoadLibraryW`, which kernel32 maps at the same address in every process of
    // a session, with the path copied into the target.
    unsafe {
        let process = OpenProcess(access, 0, pid);
        if process.is_null() {
            return Err(Error::last_os_error());
        }
        let result = (|| {
            let (mut emulated, mut native) = (0u16, 0u16);
            if IsWow64Process2(process, &mut emulated, &mut native) == 0 {
                return Err(Error::last_os_error());
            }
            if emulated != IMAGE_FILE_MACHINE_UNKNOWN {
                return Err(Error::other("a 32-bit process"));
            }
            let remote = VirtualAllocEx(process, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() {
                return Err(Error::last_os_error());
            }
            let written = WriteProcessMemory(process, remote, path.as_ptr().cast(), bytes, std::ptr::null_mut()) != 0;
            let load =
                GetProcAddress(GetModuleHandleW(windows_sys::w!("kernel32.dll")), windows_sys::s!("LoadLibraryW"));
            let thread = match (written, load) {
                (true, Some(load)) => CreateRemoteThread(
                    process,
                    std::ptr::null(),
                    0,
                    Some(std::mem::transmute::<Proc, ThreadStart>(load)),
                    remote,
                    0,
                    std::ptr::null_mut(),
                ),
                _ => std::ptr::null_mut(),
            };
            if thread.is_null() {
                let e = Error::last_os_error();
                VirtualFreeEx(process, remote, 0, MEM_RELEASE);
                return Err(e);
            }
            let waited = WaitForSingleObject(thread, 5000) == WAIT_OBJECT_0;
            CloseHandle(thread);
            // A thread still loading has the path in use; leave its few bytes to the process.
            if !waited {
                return Err(Error::other("the game did not load the hook in time"));
            }
            VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            Ok(())
        })();
        CloseHandle(process);
        result
    }
}
