//! Small platform adapters.
//!
//! The control logic itself is pure Rust plus plain file I/O. Only signal
//! installation, process liveness and the syslog socket need OS specific code,
//! so those are isolated here and everything else stays testable on any host.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const SIGINT: i32 = 2;
pub const SIGTERM: i32 = 15;
pub const SIGHUP: i32 = 1;
pub const SIGUSR1: i32 = 10;

/// Flags written by the signal handler and read by the daemon loop.
#[derive(Clone, Default)]
pub struct Signals {
    pub stop: Arc<AtomicBool>,
    pub reload: Arc<AtomicBool>,
}

impl Signals {
    pub fn new() -> Self {
        Signals::default()
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn take_reload(&self) -> bool {
        self.reload.swap(false, Ordering::SeqCst)
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{AtomicBool, Ordering, SIGHUP, SIGINT, SIGTERM, SIGUSR1};
    use std::sync::atomic::AtomicPtr;

    static STOP_FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());
    static RELOAD_FLAG: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

    extern "C" {
        /// `signal(2)` is used instead of `sigaction` because the layout of
        /// `struct sigaction` differs between musl and glibc. `signal()` is a
        /// stable ABI on both and installs with `SA_RESTART`, which is fine:
        /// the daemon sleeps in short slices rather than one long sleep.
        fn signal(signum: i32, handler: usize) -> usize;
    }

    const SIG_ERR: usize = usize::MAX;

    extern "C" fn on_signal(number: i32) {
        // Async-signal-safe: only a relaxed store into a pre-allocated flag.
        match number {
            SIGTERM | SIGINT => {
                let ptr = STOP_FLAG.load(Ordering::Relaxed);
                if !ptr.is_null() {
                    unsafe { (*ptr).store(true, Ordering::SeqCst) };
                }
            }
            SIGHUP | SIGUSR1 => {
                let ptr = RELOAD_FLAG.load(Ordering::Relaxed);
                if !ptr.is_null() {
                    unsafe { (*ptr).store(true, Ordering::SeqCst) };
                }
            }
            _ => {}
        }
    }

    pub fn install(stop: &AtomicBool, reload: &AtomicBool) {
        STOP_FLAG.store(
            stop as *const AtomicBool as *mut AtomicBool,
            Ordering::Relaxed,
        );
        RELOAD_FLAG.store(
            reload as *const AtomicBool as *mut AtomicBool,
            Ordering::Relaxed,
        );
        let handler = on_signal as *const () as usize;
        unsafe {
            for number in [SIGTERM, SIGINT, SIGHUP, SIGUSR1] {
                if signal(number, handler) == SIG_ERR {
                    eprintln!("h5000m-fancontrol: cannot install handler for signal {number}");
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use super::{AtomicBool, Ordering};

    /// Non-Linux hosts only ever build this crate for tests; there is no
    /// procd to signal us.
    pub fn install(_stop: &AtomicBool, _reload: &AtomicBool) {
        let _ = Ordering::Relaxed;
    }
}

pub fn install_handlers(signals: &Signals) {
    linux::install(&signals.stop, &signals.reload);
}

/// Wall clock seconds, the unit every persisted timestamp uses.
pub fn wall_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

pub fn sleep_slice(total: Duration, signals: &Signals, wake: &dyn Fn() -> bool) {
    let deadline = Instant::now() + total;
    loop {
        if signals.stop_requested() || wake() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

/// True when `pid` still exists and still looks like this controller.
///
/// `/proc/<pid>/cmdline` is checked as well so that a recycled PID from an
/// unrelated process cannot make a stale lock look alive. When `/proc` is not
/// mounted at all the process is assumed to be alive: guessing wrong in the
/// other direction would let two controllers race for the same PWM node.
pub fn controller_is_alive(proc_root: &Path, pid: u32, marker: &str) -> bool {
    if !proc_root.is_dir() {
        return true;
    }
    let dir = proc_root.join(pid.to_string());
    if !dir.exists() {
        return false;
    }
    match std::fs::read(proc_root.join(pid.to_string()).join("cmdline")) {
        Ok(raw) => {
            if raw.is_empty() {
                // Kernel threads have an empty cmdline; they are never us.
                return false;
            }
            let text = raw
                .split(|b| *b == 0)
                .filter(|part| !part.is_empty())
                .map(|part| String::from_utf8_lossy(part).to_string())
                .collect::<Vec<_>>()
                .join(" ");
            text.contains(marker)
        }
        // A live process we cannot introspect (permissions) is treated as
        // alive: stealing the lock of a running controller would let two
        // writers race for the same PWM node.
        Err(_) => true,
    }
}

/// Look up an executable on `PATH`, without ever running a shell.
pub fn which(binary: &str) -> Option<std::path::PathBuf> {
    if binary.contains('/') {
        let path = std::path::PathBuf::from(binary);
        return if path.exists() { Some(path) } else { None };
    }
    let value = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&value) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedRun {
    /// Process exited within the deadline; payload is stdout.
    Completed(String),
    /// Process was still running at the deadline and was killed.
    TimedOut,
    /// Process could not be started or produced an error status.
    Failed,
}

/// Run a helper with a hard deadline.
///
/// `std` has no wait-timeout, so the child is polled and explicitly killed and
/// reaped. A wedged vendor driver ioctl therefore cannot stall the thermal
/// control loop, and no zombie is left behind.
pub fn run_bounded(program: &str, args: &[&str], limit: Duration) -> BoundedRun {
    let Some(resolved) = which(program) else {
        return BoundedRun::Failed;
    };
    let spawned = Command::new(resolved)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child: Child = match spawned {
        Ok(child) => child,
        Err(_) => return BoundedRun::Failed,
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return match child.wait_with_output() {
                    Ok(out) if out.status.success() => {
                        BoundedRun::Completed(String::from_utf8_lossy(&out.stdout).to_string())
                    }
                    _ => BoundedRun::Failed,
                };
            }
            Ok(None) => {
                if started.elapsed() >= limit {
                    let _ = child.kill();
                    let _ = child.wait();
                    return BoundedRun::TimedOut;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return BoundedRun::Failed;
            }
        }
    }
}

/// Structured process start marker used by `status` to detect a restarted
/// daemon without relying on the PID alone.
pub fn process_id() -> u32 {
    std::process::id()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_binary_is_reported_as_failure_not_panic() {
        assert_eq!(
            run_bounded(
                "h5000m-definitely-not-installed",
                &[],
                Duration::from_millis(50)
            ),
            BoundedRun::Failed
        );
    }

    #[test]
    fn dead_pid_is_not_alive() {
        let root = std::env::temp_dir().join(format!("h5fan-proc-{}", wall_now()));
        let _ = fs::remove_dir_all(&root);
        // Without a proc mount nothing can be introspected; assume alive so a
        // lock is never stolen from a possibly running controller.
        assert!(controller_is_alive(&root, 424242, "h5000m-fancontrol"));
        fs::create_dir_all(&root).unwrap();
        assert!(!controller_is_alive(&root, 424242, "h5000m-fancontrol"));
        fs::create_dir_all(root.join("424242")).unwrap();
        fs::write(
            root.join("424242/cmdline"),
            b"/usr/sbin/other-daemon\0--x\0",
        )
        .unwrap();
        assert!(!controller_is_alive(&root, 424242, "h5000m-fancontrol"));
        fs::write(
            root.join("424242/cmdline"),
            b"/usr/sbin/h5000m-fancontrol\0daemon\0",
        )
        .unwrap();
        assert!(controller_is_alive(&root, 424242, "h5000m-fancontrol"));
        let _ = fs::remove_dir_all(&root);
    }
}
