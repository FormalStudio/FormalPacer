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

fn user() -> io::Result<String> {
    match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(user)) => Ok(format!("{domain}\\{user}")),
        (_, Ok(user)) => Ok(user),
        _ => Err(io::Error::new(io::ErrorKind::NotFound, "no USERNAME")),
    }
}

/// Whether this user is in Performance Log Users, which can be true before this sign-in's token
/// carries the group.
pub fn in_performance_log_users() -> bool {
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        LG_INCLUDE_INDIRECT, LOCALGROUP_USERS_INFO_0, MAX_PREFERRED_LENGTH, NERR_Success, NetApiBufferFree,
        NetUserGetLocalGroups,
    };
    let (Ok(user), Some(group)) = (user(), group_name()) else { return false };
    let user: Vec<u16> = user.encode_utf16().chain([0]).collect();
    let mut buf: *mut u8 = std::ptr::null_mut();
    let (mut read, mut total) = (0u32, 0u32);
    // SAFETY: a NUL-terminated name; the buffer NetAPI allocates is read as the array of `read`
    // entries it says it is, then freed.
    unsafe {
        let status = NetUserGetLocalGroups(
            std::ptr::null(),
            user.as_ptr(),
            0,
            LG_INCLUDE_INDIRECT,
            &mut buf,
            MAX_PREFERRED_LENGTH,
            &mut read,
            &mut total,
        );
        if status != NERR_Success || buf.is_null() {
            return false;
        }
        let groups = std::slice::from_raw_parts(buf.cast::<LOCALGROUP_USERS_INFO_0>(), read as usize);
        let found = groups.iter().any(|g| wide_str(g.lgrui0_name).eq_ignore_ascii_case(&group));
        NetApiBufferFree(buf.cast());
        found
    }
}

/// The group's name in the system's language.
fn group_name() -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{LookupAccountSidW, PSID};
    let sid_text: Vec<u16> = GROUP_SID.encode_utf16().chain([0]).collect();
    let mut sid: PSID = std::ptr::null_mut();
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len, mut kind) = (name.len() as u32, domain.len() as u32, 0);
    // SAFETY: a NUL-terminated SID string; the SID it allocates is freed, and the buffers are of
    // the lengths passed.
    unsafe {
        if ConvertStringSidToSidW(sid_text.as_ptr(), &mut sid) == 0 {
            return None;
        }
        let found = LookupAccountSidW(
            std::ptr::null(),
            sid,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        ) != 0;
        LocalFree(sid);
        found.then(|| String::from_utf16_lossy(&name[..name_len as usize]))
    }
}

/// # Safety
/// `s` is null or a NUL-terminated UTF-16 string.
unsafe fn wide_str(s: *const u16) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    let len = (0..).take_while(|&i| unsafe { *s.add(i) } != 0).count();
    // SAFETY: `len` units up to the terminator.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(s, len) })
}

pub fn join_performance_log_users() -> io::Result<()> {
    let user = user()?;
    // Joining twice is not a failure: the first join only applies from the next sign-in. A caught
    // error still leaves `-Command` exiting with 1, hence the explicit exit.
    let script = format!(
        "try {{ Add-LocalGroupMember -SID '{GROUP_SID}' -Member '{}' -ErrorAction Stop }} \
         catch {{ if ($_.FullyQualifiedErrorId -notlike 'MemberExists*') {{ throw }} }}; exit 0",
        user.replace('\'', "''")
    );
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
