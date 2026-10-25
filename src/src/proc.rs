//! Subprocess helpers: bounded execution and pid liveness.
//!
//! The historical shell controller relied on the `timeout` binary to bound
//! vendor `iwpriv` calls. The Rust controller bounds them natively, so a
//! hanging driver ioctl can never stall the safety control loop.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs a program, capturing stdout, and gives up after `timeout`.
/// Returns `None` when the program is missing, fails to spawn, or exceeds the
/// deadline (the child is killed in that case). Stderr is discarded, matching
/// the `2>/dev/null` behaviour of the original shell calls.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    Some(out)
}

/// Returns true when a process with this pid exists. Non-numeric or empty
/// input counts as dead so a corrupt lock pid file gets reclaimed. The
/// current process always counts as alive, which also keeps the lock
/// self-check correct on platforms without a `/proc` filesystem.
pub fn pid_alive(pid: u32) -> bool {
    pid == std::process::id() || Path::new(&format!("/proc/{pid}")).exists()
}

/// Searches `PATH` for an executable, equivalent to `command -v`.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let candidate_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(&candidate_name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        if cfg!(windows) && candidate_name != name {
            // Also accept extension-less matches for test doubles.
            let bare = dir.join(name);
            if is_executable(&bare) {
                return Some(bare);
            }
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_timeout_returns_output() {
        #[cfg(unix)]
        let (program, args) = ("/bin/sh", vec!["-c", "echo hello"]);
        #[cfg(windows)]
        let (program, args) = ("cmd", vec!["/C", "echo hello"]);
        let out = run_with_timeout(program, &args, Duration::from_secs(5));
        assert!(out.is_some());
        assert!(out.unwrap().contains("hello"));
    }

    #[test]
    fn run_with_timeout_kills_hanging_process() {
        #[cfg(unix)]
        let (program, args) = ("/bin/sh", vec!["-c", "sleep 5"]);
        #[cfg(windows)]
        let (program, args) = ("cmd", vec!["/C", "ping -n 5 127.0.0.1 > NUL"]);
        let start = Instant::now();
        let out = run_with_timeout(program, &args, Duration::from_millis(300));
        assert!(out.is_none());
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn missing_program_is_none() {
        assert!(run_with_timeout(
            "h5000m-definitely-missing-binary",
            &[],
            Duration::from_secs(1)
        )
        .is_none());
    }
}
