//! One page per presenting game process, in `/dev/shm`. The Vulkan layer creates it on the
//! game's first present and writes frame samples; the app reads the samples and writes back the
//! frame interval to hold. `/dev/shm` is used rather than `$XDG_RUNTIME_DIR` because Steam's
//! pressure-vessel container shares `/dev` with the host but not all of the runtime dir.
//!
//! Every field is an atomic, so either side can die at any point without leaving the other
//! with a torn value. Times are `CLOCK_MONOTONIC` nanoseconds, which is one clock for every
//! process on the machine whatever namespace it runs in.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const MAGIC: u64 = u64::from_le_bytes(*b"FPACER01");
pub const VERSION: u32 = 2;
/// Frame samples kept per process: about 3.5 s at 144 fps, enough for the app's windows.
pub const RING: usize = 512;
const NAME_LEN: usize = 64;

#[repr(C)]
pub struct Page {
    magic: AtomicU64,
    version: AtomicU32,
    pid: AtomicU32,
    steam_app_id: AtomicU32,
    /// The X11 window the game created its swapchain surface for, 0 for none.
    x11_window: AtomicU32,
    name: [AtomicU64; NAME_LEN / 8],
    /// Interval to hold in ns, written by the app; 0 means uncapped.
    target_ns: AtomicU64,
    /// When the app last wrote `target_ns`. The layer drops a cap the app stopped refreshing.
    target_stamp_ns: AtomicU64,
    /// When the last frame was presented.
    heartbeat_ns: AtomicU64,
    frames: AtomicU64,
    /// Size of the game's swapchain, written by the layer.
    swap_width: AtomicU32,
    swap_height: AtomicU32,
    /// Bumped by the layer each time the overlay hotkey is pressed; the app flips the setting.
    toggles: AtomicU32,
    /// Written by the app: [`Overlay`] as bits.
    overlay: AtomicU32,
    ring: [AtomicU64; RING],
}

// 32-bit and 64-bit games share pages with the 64-bit app, so the layout must not depend on the
// target. Every field is a fixed-size atomic; this pins the total.
const _: () = assert!(size_of::<Page>() == 136 + 8 * RING);

/// What the app asks the in-game overlay to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub show: bool,
    pub vrr: VrrState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VrrState {
    #[default]
    Unknown,
    Unsupported,
    Off,
    On,
}

impl Overlay {
    fn pack(self) -> u32 {
        let vrr = match self.vrr {
            VrrState::Unknown => 0,
            VrrState::Unsupported => 1,
            VrrState::Off => 2,
            VrrState::On => 3,
        };
        u32::from(self.show) | (vrr << 1)
    }

    fn unpack(v: u32) -> Self {
        let vrr = match (v >> 1) & 3 {
            1 => VrrState::Unsupported,
            2 => VrrState::Off,
            3 => VrrState::On,
            _ => VrrState::Unknown,
        };
        Self { show: v & 1 != 0, vrr }
    }
}

/// One presented frame: the interval since the previous present and how much of it the layer
/// spent sleeping. `interval - slept` is what the game itself needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub interval_us: u32,
    pub slept_us: u32,
}

impl Sample {
    pub fn work_us(self) -> u32 {
        self.interval_us.saturating_sub(self.slept_us)
    }

    fn pack(self) -> u64 {
        (u64::from(self.interval_us) << 32) | u64::from(self.slept_us)
    }

    fn unpack(v: u64) -> Self {
        Self { interval_us: (v >> 32) as u32, slept_us: v as u32 }
    }
}

pub fn now_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes the timespec it is given.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub fn shm_dir() -> PathBuf {
    std::env::var_os("FORMALPACER_SHM_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/dev/shm"))
}

/// File name prefix for this user's pages.
pub fn prefix() -> String {
    // SAFETY: getuid cannot fail.
    format!("formalpacer-{}-", unsafe { libc::getuid() })
}

/// A mapped page. Unmapped on drop; the file stays until [`Mapping::unlink`] or the app's sweep.
pub struct Mapping {
    page: NonNull<Page>,
    path: PathBuf,
}

// SAFETY: the page holds only atomics.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Creates this process's page. Called by the layer.
    pub fn create(name: &str, steam_app_id: u32) -> io::Result<Self> {
        let pid = std::process::id();
        let path = shm_dir().join(format!("{}{pid}-{:x}", prefix(), now_ns()));
        let map = Self::open(&path, true)?;
        let page = map.page();
        let mut bytes = [0u8; NAME_LEN];
        let len = name.len().min(NAME_LEN - 1);
        bytes[..len].copy_from_slice(&name.as_bytes()[..len]);
        for (slot, chunk) in page.name.iter().zip(bytes.chunks_exact(8)) {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            slot.store(u64::from_le_bytes(word), Ordering::Relaxed);
        }
        page.pid.store(pid, Ordering::Relaxed);
        page.steam_app_id.store(steam_app_id, Ordering::Relaxed);
        page.version.store(VERSION, Ordering::Relaxed);
        page.magic.store(MAGIC, Ordering::Release);
        Ok(map)
    }

    /// Maps an existing page. Called by the app; fails on a page from another version.
    pub fn attach(path: &Path) -> io::Result<Self> {
        let map = Self::open(path, false)?;
        let page = map.page();
        if page.magic.load(Ordering::Acquire) != MAGIC || page.version.load(Ordering::Relaxed) != VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a FormalPacer page of this version"));
        }
        Ok(map)
    }

    fn open(path: &Path, create: bool) -> io::Result<Self> {
        let c_path = CString::new(path.as_os_str().as_bytes())?;
        let flags =
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW | if create { libc::O_CREAT | libc::O_EXCL } else { 0 };
        // SAFETY: plain syscalls on a path we own; every result is checked.
        unsafe {
            let fd = libc::open(c_path.as_ptr(), flags, 0o600);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let size = size_of::<Page>();
            if create && libc::ftruncate(fd, size as libc::off_t) != 0 {
                let err = io::Error::last_os_error();
                libc::close(fd);
                return Err(err);
            }
            let mut st: libc::stat = std::mem::zeroed();
            // /dev/shm is shared by every user: only a regular file of ours that nobody else can
            // write is a page.
            let stat_ok = libc::fstat(fd, &mut st) == 0;
            let ours =
                st.st_mode & libc::S_IFMT == libc::S_IFREG && st.st_uid == libc::getuid() && st.st_mode & 0o077 == 0;
            if !stat_ok || !ours || (st.st_size as usize) < size {
                libc::close(fd);
                return Err(io::Error::new(io::ErrorKind::InvalidData, "not a page of this user's"));
            }
            let ptr =
                libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            let page = NonNull::new(ptr.cast::<Page>()).ok_or_else(io::Error::last_os_error)?;
            Ok(Self { page, path: path.to_path_buf() })
        }
    }

    fn page(&self) -> &Page {
        // SAFETY: mapped for our lifetime, and every field is valid for any bit pattern.
        unsafe { self.page.as_ref() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn unlink(&self) {
        let _ = std::fs::remove_file(&self.path);
    }

    pub fn pid(&self) -> u32 {
        self.page().pid.load(Ordering::Relaxed)
    }

    pub fn steam_app_id(&self) -> u32 {
        self.page().steam_app_id.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> String {
        let bytes: Vec<u8> = self.page().name.iter().flat_map(|w| w.load(Ordering::Relaxed).to_le_bytes()).collect();
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    pub fn x11_window(&self) -> u32 {
        self.page().x11_window.load(Ordering::Relaxed)
    }

    pub fn set_x11_window(&self, window: u32) {
        self.page().x11_window.store(window, Ordering::Relaxed);
    }

    pub fn heartbeat_ns(&self) -> u64 {
        self.page().heartbeat_ns.load(Ordering::Relaxed)
    }

    pub fn frames(&self) -> u64 {
        self.page().frames.load(Ordering::Acquire)
    }

    /// Records a presented frame. Single writer per page (the layer serialises presents).
    pub fn push(&self, sample: Sample, now: u64) {
        let page = self.page();
        let n = page.frames.load(Ordering::Relaxed);
        page.ring[(n % RING as u64) as usize].store(sample.pack(), Ordering::Relaxed);
        page.heartbeat_ns.store(now, Ordering::Relaxed);
        page.frames.store(n + 1, Ordering::Release);
    }

    /// Samples pushed after frame `since` (at most the ring's worth), oldest first, and the
    /// frame count they run up to.
    pub fn samples_since(&self, since: u64) -> (Vec<Sample>, u64) {
        let page = self.page();
        let end = page.frames.load(Ordering::Acquire);
        let start = since.max(end.saturating_sub(RING as u64 - 1));
        let samples = (start..end)
            .map(|i| Sample::unpack(page.ring[(i % RING as u64) as usize].load(Ordering::Relaxed)))
            .collect();
        (samples, end)
    }

    pub fn swap_extent(&self) -> (u32, u32) {
        let page = self.page();
        (page.swap_width.load(Ordering::Relaxed), page.swap_height.load(Ordering::Relaxed))
    }

    pub fn set_swap_extent(&self, width: u32, height: u32) {
        let page = self.page();
        page.swap_width.store(width, Ordering::Relaxed);
        page.swap_height.store(height, Ordering::Relaxed);
    }

    pub fn toggles(&self) -> u32 {
        self.page().toggles.load(Ordering::Relaxed)
    }

    pub fn request_toggle(&self) {
        self.page().toggles.fetch_add(1, Ordering::Relaxed);
    }

    pub fn overlay(&self) -> Overlay {
        Overlay::unpack(self.page().overlay.load(Ordering::Relaxed))
    }

    pub fn set_overlay(&self, overlay: Overlay) {
        self.page().overlay.store(overlay.pack(), Ordering::Relaxed);
    }

    pub fn set_target(&self, interval_ns: u64, now: u64) {
        let page = self.page();
        page.target_ns.store(interval_ns, Ordering::Relaxed);
        page.target_stamp_ns.store(now, Ordering::Release);
    }

    /// The interval the app asked for, or 0 when it asked for none or stopped refreshing it
    /// (closed or crashed) for longer than `stale_ns`.
    pub fn target(&self, now: u64, stale_ns: u64) -> u64 {
        let page = self.page();
        let stamp = page.target_stamp_ns.load(Ordering::Acquire);
        if now.saturating_sub(stamp) > stale_ns { 0 } else { page.target_ns.load(Ordering::Relaxed) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `open` mapped.
        unsafe { libc::munmap(self.page.as_ptr().cast(), size_of::<Page>()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_and_app_see_the_same_page() {
        let dir = std::env::temp_dir().join(format!("fpshm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test setup.
        unsafe { std::env::set_var("FORMALPACER_SHM_DIR", &dir) };
        let layer = Mapping::create("eldenring.exe", 1245620).unwrap();
        let app = Mapping::attach(layer.path()).unwrap();
        assert_eq!(app.name(), "eldenring.exe");
        assert_eq!(app.steam_app_id(), 1245620);

        for i in 0..(RING as u32 + 10) {
            layer.push(Sample { interval_us: 7000 + i, slept_us: 1000 }, 5);
        }
        let (samples, end) = app.samples_since(0);
        assert_eq!(end, RING as u64 + 10);
        assert_eq!(samples.len(), RING - 1);
        assert_eq!(samples.last().unwrap().interval_us, 7000 + RING as u32 + 9);
        assert_eq!(samples[0].work_us(), samples[0].interval_us - 1000);
        assert_eq!(app.samples_since(end - 2).0.len(), 2);

        let overlay = Overlay { show: true, vrr: VrrState::On };
        app.set_overlay(overlay);
        assert_eq!(layer.overlay(), overlay);
        layer.request_toggle();
        assert_eq!(app.toggles(), 1);

        app.set_target(6_944_444, 100);
        assert_eq!(layer.target(200, 1_000), 6_944_444);
        assert_eq!(layer.target(5_000, 1_000), 0);
        layer.unlink();

        // A page anyone else can write, or a link to one, is refused.
        let open = dir.join(format!("{}open", prefix()));
        let linked = dir.join(format!("{}linked", prefix()));
        let page = Mapping::create("x", 0).unwrap();
        std::fs::rename(page.path(), &open).unwrap();
        std::os::unix::fs::symlink(&open, &linked).unwrap();
        assert!(Mapping::attach(&linked).is_err());
        assert!(Mapping::attach(&open).is_ok());
        std::fs::set_permissions(&open, std::os::unix::fs::PermissionsExt::from_mode(0o666)).unwrap();
        assert!(Mapping::attach(&open).is_err());
        std::fs::remove_file(&open).unwrap();
        std::fs::remove_file(&linked).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
