use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

pub fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub fn default_dir() -> PathBuf {
    PathBuf::from("/dev/shm")
}

pub fn prefix() -> String {
    // SAFETY: getuid cannot fail.
    format!("formalpacer-{}-", unsafe { libc::getuid() })
}

pub fn map(path: &Path, create: bool, size: usize) -> io::Result<NonNull<u8>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let flags =
        libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | if create { libc::O_CREAT | libc::O_EXCL } else { 0 };
    // SAFETY: plain syscalls on a path we own; every result is checked.
    unsafe {
        let fd = libc::open(c_path.as_ptr(), flags, 0o600);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if create && libc::ftruncate(fd, size as libc::off_t) != 0 {
            let err = io::Error::last_os_error();
            libc::close(fd);
            return Err(err);
        }
        let mut st: libc::stat = std::mem::zeroed();
        // /dev/shm is shared by every user: only a regular file of ours that nobody else can
        // write is a page.
        let stat_ok = libc::fstat(fd, &mut st) == 0;
        let ours = st.st_mode & libc::S_IFMT == libc::S_IFREG && st.st_uid == libc::getuid() && st.st_mode & 0o077 == 0;
        if !stat_ok || !ours || (st.st_size as usize) < size {
            libc::close(fd);
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a page of this user's"));
        }
        let ptr = libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        libc::close(fd);
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        NonNull::new(ptr.cast()).ok_or_else(io::Error::last_os_error)
    }
}

pub fn unmap(ptr: NonNull<u8>, size: usize) {
    // SAFETY: unmaps exactly what `map` mapped.
    unsafe { libc::munmap(ptr.as_ptr().cast(), size) };
}
