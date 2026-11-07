//! Frame times of every presenting process on Windows, from Event Tracing for Windows, through
//! Intel's PresentMon running as a hidden child process. Nothing is loaded into the game, which is
//! what makes this the only way to measure a game under a kernel anti-cheat. PresentMon needs an
//! administrator or a member of the Performance Log Users group; anyone else gets
//! [`State::Denied`].
//!
//! Each row is one present. A frame's work is the longer of the time the CPU spent on it
//! (`CPUBusy`) and the time the GPU did (`GPUBusy`); the rest of the frame (`FrameTime`) was spent
//! waiting, on a limiter, vsync or the GPU queue. That is the same split the layer reports as
//! interval and sleep, so the planner reads both alike.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use formalpacer_shm::{Sample, now_ns};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const EXE: &str = "PresentMon.exe";
const SESSION: &str = "FormalPacer";
/// Per process, enough for the planner's two seconds at any frame rate between two engine ticks.
const KEEP: usize = 8192;
const RESTART_AFTER: Duration = Duration::from_secs(5);

pub use crate::engine::Outside as State;

#[derive(Default)]
struct Process {
    name: String,
    samples: VecDeque<Sample>,
    frames: u64,
    heartbeat_ns: u64,
}

#[derive(Default)]
struct Shared {
    state: Option<State>,
    processes: HashMap<u32, Process>,
}

#[derive(Clone)]
pub struct Feed {
    shared: Arc<Mutex<Shared>>,
}

pub struct Presenter {
    pub pid: u32,
    pub name: String,
}

impl Feed {
    pub fn start() -> Self {
        let feed = Self { shared: Arc::default() };
        let shared = feed.shared.clone();
        let spawned =
            std::thread::Builder::new().name("formalpacer-presentmon".into()).spawn(move || supervise(&shared));
        if let Err(e) = spawned {
            feed.set_state(State::Failed(e.to_string()));
        }
        feed
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_state(&self, state: State) {
        self.lock().state = Some(state);
    }

    pub fn state(&self) -> State {
        self.lock().state.clone().unwrap_or(State::Starting)
    }

    pub fn presenters(&self) -> Vec<Presenter> {
        self.lock().processes.iter().map(|(&pid, p)| Presenter { pid, name: p.name.clone() }).collect()
    }

    pub fn heartbeat_ns(&self, pid: u32) -> u64 {
        self.lock().processes.get(&pid).map_or(0, |p| p.heartbeat_ns)
    }

    /// Samples after frame `since`, oldest first, and the frame count they run up to.
    pub fn samples_since(&self, pid: u32, since: u64) -> (Vec<Sample>, u64) {
        let shared = self.lock();
        let Some(p) = shared.processes.get(&pid) else { return (Vec::new(), since) };
        let fresh = p.frames.saturating_sub(since).min(p.samples.len() as u64) as usize;
        (p.samples.iter().skip(p.samples.len() - fresh).copied().collect(), p.frames)
    }

    pub fn forget(&self, pid: u32) {
        self.lock().processes.remove(&pid);
    }
}

fn supervise(shared: &Arc<Mutex<Shared>>) {
    let feed = Feed { shared: shared.clone() };
    let Some(exe) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(EXE))).filter(|p| p.is_file())
    else {
        feed.set_state(State::Missing);
        return;
    };
    // Closing the job, which happens when this process exits however it exits, ends PresentMon
    // and with it the trace session.
    let job = kill_on_close_job();
    loop {
        match run(&feed, &exe, job) {
            Ok(()) => feed.set_state(State::Failed("PresentMon stopped".into())),
            Err(state) => feed.set_state(state),
        }
        if feed.state() == State::Denied {
            return;
        }
        std::thread::sleep(RESTART_AFTER);
    }
}

fn kill_on_close_job() -> isize {
    // SAFETY: creates an unnamed job and sets its limit flags from a zeroed, sized struct. The
    // handle is never closed: the job lives as long as this process.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return 0;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        job as isize
    }
}

fn run(feed: &Feed, exe: &PathBuf, job: isize) -> Result<(), State> {
    let mut child = Command::new(exe)
        .args([
            "--output_stdout",
            "--no_console_stats",
            "--v2_metrics",
            "--no_track_input",
            "--stop_existing_session",
            "--session_name",
            SESSION,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| State::Failed(e.to_string()))?;
    if job != 0 {
        // SAFETY: a live job handle and the child's process handle.
        unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    }
    let mut stderr = child.stderr.take();
    let stdout = child.stdout.take().ok_or_else(|| State::Failed("no output from PresentMon".into()))?;
    let mut columns: Option<Columns> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        match &columns {
            None => {
                columns = Columns::from_header(&line);
                if columns.is_some() {
                    feed.set_state(State::Running);
                }
            }
            Some(c) => {
                if let Some((pid, name, sample)) = c.parse(&line) {
                    record(feed, pid, name, sample);
                }
            }
        }
    }
    let status = child.wait().map_err(|e| State::Failed(e.to_string()))?;
    let mut err = String::new();
    if let Some(stderr) = stderr.as_mut() {
        let _ = stderr.read_to_string(&mut err);
    }
    if err.contains("Performance Log Users") || err.to_ascii_lowercase().contains("administrator") {
        return Err(State::Denied);
    }
    if status.success() {
        Ok(())
    } else {
        Err(State::Failed(err.lines().next().unwrap_or("PresentMon failed").to_owned()))
    }
}

fn record(feed: &Feed, pid: u32, name: &str, sample: Sample) {
    let mut shared = feed.lock();
    let p = shared.processes.entry(pid).or_default();
    if p.name.is_empty() {
        name.clone_into(&mut p.name);
    }
    p.samples.push_back(sample);
    if p.samples.len() > KEEP {
        p.samples.pop_front();
    }
    p.frames += 1;
    p.heartbeat_ns = now_ns();
}

struct Columns {
    application: usize,
    pid: usize,
    frame_time: usize,
    cpu_busy: usize,
    gpu_busy: usize,
}

impl Columns {
    fn from_header(header: &str) -> Option<Self> {
        let names: Vec<&str> = header.split(',').map(str::trim).collect();
        let at = |n: &str| names.iter().position(|c| *c == n);
        Some(Self {
            application: at("Application")?,
            pid: at("ProcessID")?,
            frame_time: at("FrameTime")?,
            cpu_busy: at("CPUBusy")?,
            gpu_busy: at("GPUBusy")?,
        })
    }

    fn parse<'a>(&self, line: &'a str) -> Option<(u32, &'a str, Sample)> {
        let fields: Vec<&str> = line.split(',').collect();
        let ms = |i: usize| fields.get(i).and_then(|v| v.trim().parse::<f64>().ok());
        let pid = fields.get(self.pid)?.trim().parse().ok()?;
        let interval = ms(self.frame_time)?;
        let work = ms(self.cpu_busy).unwrap_or(0.).max(ms(self.gpu_busy).unwrap_or(0.)).min(interval);
        let us = |v: f64| (v * 1000.).round().clamp(0., f64::from(u32::MAX)) as u32;
        let sample = Sample { interval_us: us(interval), slept_us: us(interval - work) };
        Some((pid, fields.get(self.application)?.trim(), sample))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rows_by_their_header() {
        let header = "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,AllowsTearing,\
                      PresentMode,CPUStartTime,FrameTime,CPUBusy,CPUWait,GPULatency,GPUTime,GPUBusy,GPUWait";
        let c = Columns::from_header(header).unwrap();
        let row = "FortniteClient-Win64-Shipping.exe,4242,0x1,DXGI,0,0,1,Hardware: Independent Flip,\
                   1234.5,13.889,4.2,9.689,1.0,6.9,6.5,0.4";
        let (pid, name, sample) = c.parse(row).unwrap();
        assert_eq!((pid, name), (4242, "FortniteClient-Win64-Shipping.exe"));
        // GPU-bound past the CPU: the GPU's 6.5 ms is the frame's work.
        assert_eq!(sample, Sample { interval_us: 13_889, slept_us: 7_389 });
        assert!(c.parse("dwm.exe,10,0x1,Other,NA").is_none());
    }
}
