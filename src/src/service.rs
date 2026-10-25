//! Daemon lifecycle: lock acquisition, signal handling and the control loop.
//!
//! procd runs the daemon in the foreground and respawns it. SIGTERM/SIGINT
//! stop the loop cleanly (lock and thermal policy file are released, the last
//! fan output is intentionally kept), SIGHUP triggers an immediate reload.

use crate::config::Config;
use crate::controller::log;
use crate::controller::Controller;
use crate::paths::{read_trim, Paths};
use crate::sensor::IwprivVendor;
use crate::status;
use crate::thermal;
use crate::uci::UciShowSource;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SIG_HUP: i32 = 1;
const SIG_INT: i32 = 2;
const SIG_TERM: i32 = 15;

static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn on_signal(sig: i32) {
    // Only an atomic store: async-signal-safe.
    PENDING_SIGNAL.store(sig, Ordering::SeqCst);
}

#[cfg(unix)]
mod ffi {
    extern "C" {
        // Declared through an alias so the name cannot resolve to the
        // private libc re-exports inside std on musl targets.
        #[link_name = "signal"]
        pub fn c_signal(signum: i32, handler: usize) -> usize;
    }
}

pub fn install_signal_handlers() {
    #[cfg(unix)]
    {
        let handler = on_signal as *const () as usize;
        unsafe {
            ffi::c_signal(SIG_HUP, handler);
            ffi::c_signal(SIG_INT, handler);
            ffi::c_signal(SIG_TERM, handler);
        }
    }
}

fn take_signal() -> i32 {
    PENDING_SIGNAL.swap(0, Ordering::SeqCst)
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Sleeps in short slices so signals are noticed within ~200 ms; returns the
/// signal number that interrupted the sleep, or 0 after the full duration.
pub fn sleep_interruptible(total_secs: u64) -> i32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(total_secs);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return 0;
        }
        let slice = deadline - now;
        let slice = if slice > Duration::from_millis(200) {
            Duration::from_millis(200)
        } else {
            slice
        };
        std::thread::sleep(slice);
        let sig = take_signal();
        if sig != 0 {
            return sig;
        }
    }
}

/// Advisory single-instance lock backed by a directory with the owner pid,
/// reclaimable when the recorded pid is gone.
pub struct Lock {
    dir: std::path::PathBuf,
    pub held: bool,
}

impl Lock {
    pub fn acquire(paths: &Paths) -> Option<Lock> {
        let dir = paths.lock_dir.clone();
        if try_create(&dir) {
            return Some(Lock { dir, held: true });
        }
        // Reclaim stale locks whose owner is gone.
        let owner = read_trim(&dir.join("pid")).and_then(|v| v.parse::<u32>().ok());
        let alive = owner.is_some_and(crate::proc::pid_alive);
        if !alive {
            let _ = std::fs::remove_dir_all(&dir);
            if try_create(&dir) {
                return Some(Lock { dir, held: true });
            }
        }
        None
    }

    pub fn release(&mut self) {
        if self.held {
            let _ = std::fs::remove_dir_all(&self.dir);
            self.held = false;
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.release();
    }
}

fn try_create(dir: &std::path::Path) -> bool {
    // The lock lives under /var/lock on the router, but tests may point the
    // override at a fresh tree, so make sure the parent exists.
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::create_dir(dir).is_ok() {
        let _ = std::fs::write(dir.join("pid"), std::process::id().to_string());
        true
    } else {
        false
    }
}

/// Reports whether a daemon instance currently holds the lock.
pub fn daemon_running(paths: &Paths) -> bool {
    if !paths.lock_dir.is_dir() {
        return false;
    }
    read_trim(&paths.lock_dir.join("pid"))
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(crate::proc::pid_alive)
}

pub fn run_daemon(paths: &Paths) -> i32 {
    install_signal_handlers();
    let mut uci_source = UciShowSource::new();
    let mut pending: i32 = 0;
    loop {
        if pending == SIG_TERM || pending == SIG_INT {
            clean_exit(paths);
            return 0;
        }
        let reload = pending == SIG_HUP;
        pending = 0;

        let cfg = Config::load(&mut uci_source);
        if let Some(mut lock) = Lock::acquire(paths) {
            let mut vendor = IwprivVendor::new(paths.radio_root.clone());
            let controller = Controller::new(paths);
            let now = now_epoch();
            if let Err(message) = controller.apply_once(&cfg, &mut vendor, now, &|secs| {
                sleep_interruptible(secs);
            }) {
                log(&format!("control cycle failed: {message}"));
            } else {
                let _ = status::write_snapshot(paths, &cfg);
            }
            lock.release();
        }

        if !reload {
            pending = sleep_interruptible(cfg.interval as u64);
        }
    }
}

fn clean_exit(paths: &Paths) {
    if let Some(owner) = read_trim(&paths.lock_dir.join("pid")).and_then(|v| v.parse::<u32>().ok())
    {
        // Only remove the lock when we still own it.
        if owner == std::process::id() {
            let _ = std::fs::remove_dir_all(&paths.lock_dir);
        }
    }
    thermal::release_thermal_control(paths);
}

/// Writes the status snapshot next to the state file so web clients can read
/// cached sensor data instead of re-querying hardware on every poll.
pub fn write_status_snapshot(paths: &Paths) -> std::io::Result<()> {
    let mut uci_source = UciShowSource::new();
    let cfg = Config::load(&mut uci_source);
    status::write_snapshot(paths, &cfg).map_err(std::io::Error::other)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // test fixtures patch a few paths
mod tests {
    use super::*;
    use crate::state::atomic_write;

    #[test]
    fn lock_is_exclusive_and_reclaimable() {
        let tmp = std::env::temp_dir().join("h5fan-lock-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut paths = Paths::default();
        paths.lock_dir = tmp.join("h5000m-fancontrol.lock");
        std::fs::create_dir_all(paths.lock_dir.parent().unwrap()).unwrap();
        let first = Lock::acquire(&paths).unwrap();
        assert!(Lock::acquire(&paths).is_none(), "second acquire must fail");
        drop(first);
        let second = Lock::acquire(&paths).unwrap();
        // Simulate a stale owner that no longer exists.
        std::fs::write(paths.lock_dir.join("pid"), "4194304").unwrap();
        drop(second);
        std::fs::create_dir_all(&paths.lock_dir).unwrap();
        std::fs::write(paths.lock_dir.join("pid"), "4194304").unwrap();
        assert!(
            Lock::acquire(&paths).is_some(),
            "stale lock must be reclaimed"
        );
        assert!(!daemon_running(&paths), "dead pid is not a running daemon");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn interruptible_sleep_returns_quickly() {
        let start = std::time::Instant::now();
        assert_eq!(sleep_interruptible(0), 0);
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn atomic_snapshot_write() {
        let tmp = std::env::temp_dir().join("h5fan-snap-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("snap.json");
        std::fs::write(&target, "old").unwrap();
        atomic_write(&target, "{\"new\":true}").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"new\":true}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
