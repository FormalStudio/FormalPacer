//! The one thing FormalPacer asks an administrator for on Windows: adding the user to the
//! Performance Log Users group, which may start the event traces PresentMon reads. Windows shows
//! its own consent prompt; the group applies from the next sign-in.

use std::io;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

/// The group's well-known SID, S-1-5-32-559, so the command works whatever the system language
/// calls the group.
const GROUP_SID: &str = "S-1-5-32-559";

pub fn join_performance_log_users() -> io::Result<()> {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(user)) => format!("{domain}\\{user}"),
        (_, Ok(user)) => user,
        _ => return Err(io::Error::new(io::ErrorKind::NotFound, "no USERNAME")),
    };
    let script =
        format!("Add-LocalGroupMember -SID '{GROUP_SID}' -Member '{}' -ErrorAction Stop", user.replace('\'', "''"));
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let verb = wide("runas");
    let file = wide("powershell.exe");
    let params = wide(&format!("-NoProfile -NonInteractive -Command \"{script}\""));
    // SAFETY: plain data, valid when zeroed.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: a sized struct whose strings outlive the call; the process handle it returns is
    // waited on and closed.
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.hProcess.is_null() {
            return Err(io::Error::other("no process started"));
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if code != 0 {
            return Err(io::Error::other(format!("PowerShell exited with {code}")));
        }
    }
    Ok(())
}
