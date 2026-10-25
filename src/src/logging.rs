//! Log emission.
//!
//! Messages go to the `LOG_DAEMON` syslog socket directly instead of forking
//! `logger`, and to stderr as a fallback so `procd` still captures them when
//! `/dev/log` is not up yet.

use crate::events::Level;
use crate::paths::Paths;
use std::io::Write;

#[cfg(unix)]
mod socket {
    use std::os::unix::net::UnixDatagram;
    use std::path::Path;

    pub fn send(priority: u8, payload: &str, socket_path: &Path) -> bool {
        let Ok(socket) = UnixDatagram::unbound() else {
            return false;
        };
        let message = format!("<{priority}>{payload}");
        // A single datagram per record; logd truncates anything larger.
        socket.send_to(message.as_bytes(), socket_path).is_ok()
    }
}

#[cfg(not(unix))]
mod socket {
    use std::path::Path;

    pub fn send(_priority: u8, _payload: &str, _socket_path: &Path) -> bool {
        false
    }
}

/// Write one record. `pid` is included so an operator can tell daemon restarts
/// apart in the log.
pub fn emit(paths: &Paths, level: Level, pid: u32, message: &str) {
    let payload = format!("h5000m-fancontrol[{pid}]: {message}");
    if socket::send(level.syslog_priority(), &payload, &paths.syslog_socket) {
        return;
    }
    let stderr = std::io::stderr();
    let mut handle = stderr.lock();
    let _ = writeln!(handle, "{} {payload}", level.as_str());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform;

    #[test]
    fn emit_falls_back_to_stderr_when_no_socket_exists() {
        let root = std::env::temp_dir().join(format!(
            "h5fan-log-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let paths = Paths::sandbox(root.clone());
        // Must not panic even though neither the socket nor its parent exists.
        emit(&paths, Level::Warn, 1, "fallback path");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn priorities_map_to_log_daemon_facility() {
        assert_eq!(Level::Info.syslog_priority(), 3 * 8 + 6);
        assert_eq!(Level::Warn.syslog_priority(), 3 * 8 + 4);
        assert_eq!(Level::Error.syslog_priority(), 3 * 8 + 3);
    }
}
