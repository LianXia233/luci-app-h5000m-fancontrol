//! Exclusive control lock.
//!
//! Same directory and same `pid` file as the 2.x shell controller, so an old
//! daemon and a new one cannot both drive the fan during an upgrade. The lock is
//! now held for the whole daemon lifetime instead of per cycle, which removes
//! the old window where `apply` and the loop could interleave PWM writes.

use crate::error::{Error, Result};
use crate::paths::{self, Paths};
use crate::platform;
use std::path::{Path, PathBuf};

/// String looked for in `/proc/<pid>/cmdline` before a lock is declared stale.
pub const PROCESS_MARKER: &str = "h5000m-fancontrol";

#[derive(Debug)]
pub enum Acquisition {
    /// We own the lock now.
    Acquired(Lock),
    /// A live controller process owns it; `apply` should nudge instead of race.
    Held { pid: u32 },
    /// The lock directory can neither be created nor reclaimed.
    Unavailable { message: String },
}

/// Guard for the exclusive control lock directory.
///
/// It only keeps the directory itself, so holding a lock does not pin the whole
/// path configuration, and releasing is possible from `Drop`.
#[derive(Debug)]
pub struct Lock {
    dir: PathBuf,
}

fn read_pid(dir: &Path) -> Option<u32> {
    paths::read_trimmed(&dir.join("pid")).and_then(|raw| raw.parse::<u32>().ok())
}

fn claim(dir: &Path) -> Result<()> {
    // The parent chain (/var/lock) must exist, but the lock directory itself is
    // created exclusively: create_dir_all would succeed for a directory another
    // controller already owns.
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::create_dir(dir).map_err(|err| Error::io(dir.display().to_string(), &err))?;
    std::fs::write(dir.join("pid"), format!("{}\n", std::process::id()))
        .map_err(|err| Error::io(dir.display().to_string(), &err))?;
    Ok(())
}

impl Lock {
    /// Never blocks: the caller decides whether to wait or to nudge the daemon.
    pub fn try_acquire(paths: &Paths) -> Acquisition {
        let dir = &paths.lock_dir;
        if claim(dir).is_ok() {
            return Acquisition::Acquired(Lock { dir: dir.clone() });
        }
        let owner = read_pid(dir);
        let alive = owner
            .filter(|pid| platform::controller_is_alive(&paths.proc_root, *pid, PROCESS_MARKER));
        if let Some(pid) = alive {
            return Acquisition::Held { pid };
        }
        // No pid, an unparsable one, or a process that is gone: reclaim it.
        let _ = std::fs::remove_dir_all(dir);
        match claim(dir) {
            Ok(()) => Acquisition::Acquired(Lock { dir: dir.clone() }),
            Err(err) => Acquisition::Unavailable {
                message: err.to_string(),
            },
        }
    }

    /// Pid of the live process holding the lock, if any.
    pub fn holder(paths: &Paths) -> Option<u32> {
        let pid = read_pid(&paths.lock_dir)?;
        if platform::controller_is_alive(&paths.proc_root, pid, PROCESS_MARKER) {
            Some(pid)
        } else {
            None
        }
    }

    pub fn release(&self) {
        // Only remove our own lock: never delete one another process created.
        if read_pid(&self.dir) == Some(std::process::id()) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(label: &str) -> Paths {
        let root = std::env::temp_dir().join(format!(
            "h5fan-lock-{label}-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let paths = Paths::sandbox(root);
        // A fake proc mount that knows only about this process, so liveness
        // decisions are exercised instead of falling back to "assume alive".
        let self_dir = paths.proc_root.join(std::process::id().to_string());
        std::fs::create_dir_all(&self_dir).unwrap();
        std::fs::write(
            self_dir.join("cmdline"),
            b"/usr/sbin/h5000m-fancontrol\0daemon\0",
        )
        .unwrap();
        paths
    }

    #[test]
    fn lock_is_exclusive_for_the_process_that_holds_it() {
        let paths = temp_paths("exclusive");
        let lock = match Lock::try_acquire(&paths) {
            Acquisition::Acquired(lock) => lock,
            other => panic!("first acquisition must succeed, got {other:?}"),
        };
        assert!(Lock::holder(&paths).is_some());
        match Lock::try_acquire(&paths) {
            Acquisition::Held { pid } => assert_eq!(pid, std::process::id()),
            other => panic!("second acquisition must fail, got {other:?}"),
        }
        drop(lock);
        assert!(
            !paths.lock_dir.exists(),
            "a released lock directory is removed"
        );
    }

    #[test]
    fn stale_lock_from_a_dead_pid_is_reclaimed() {
        let paths = temp_paths("stale");
        std::fs::create_dir_all(&paths.lock_dir).unwrap();
        std::fs::write(paths.lock_dir.join("pid"), "4242424\n").unwrap();
        match Lock::try_acquire(&paths) {
            Acquisition::Acquired(lock) => {
                assert_eq!(read_pid(&paths.lock_dir), Some(std::process::id()));
                drop(lock);
            }
            other => panic!("stale lock must be reclaimed, got {other:?}"),
        }
    }

    #[test]
    fn lock_with_garbage_pid_is_reclaimed() {
        let paths = temp_paths("garbage");
        std::fs::create_dir_all(&paths.lock_dir).unwrap();
        std::fs::write(paths.lock_dir.join("pid"), "not-a-pid\n").unwrap();
        assert!(
            matches!(Lock::try_acquire(&paths), Acquisition::Acquired(_)),
            "an unparsable pid is not a live owner"
        );
    }

    #[test]
    fn holder_of_a_dead_process_is_not_reported() {
        let paths = temp_paths("holder");
        std::fs::create_dir_all(&paths.lock_dir).unwrap();
        std::fs::write(paths.lock_dir.join("pid"), "999999\n").unwrap();
        assert_eq!(Lock::holder(&paths), None);
    }

    #[test]
    fn dropping_someone_elses_lock_is_not_possible() {
        let paths = temp_paths("foreign");
        std::fs::create_dir_all(&paths.lock_dir).unwrap();
        std::fs::write(paths.lock_dir.join("pid"), "999999\n").unwrap();
        let lock = Lock {
            dir: paths.lock_dir.clone(),
        };
        drop(lock);
        assert!(
            paths.lock_dir.exists(),
            "a lock owned by another pid survives our drop"
        );
    }
}
