//! Filesystem roots used by the controller.
//!
//! Every hardware facing path is resolved through [`Paths`] instead of a string
//! literal so that the whole control loop can run against a simulated sysfs
//! tree in tests. The environment variable names keep the ones the 2.x shell
//! controller already exposed.

use std::path::{Path, PathBuf};

/// Directory entry names such as `hwmon10` sort lexicographically before
/// `hwmon2`, which would change which sensor wins when several candidates
/// exist. Sorting by the embedded index keeps discovery stable.
pub fn sort_by_trailing_index(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let key = |p: &Path| -> (u128, String) {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let digits: String = name
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let index = digits.parse::<u128>().unwrap_or(u128::MAX);
            (index, name)
        };
        key(a).cmp(&key(b))
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Root of `/sys`; thermal zones and hwmon devices are resolved below it.
    pub sysfs: PathBuf,
    /// Root holding network interfaces, used to check radio interfaces exist.
    pub radio: PathBuf,
    /// `/sys/firmware/devicetree/base` equivalent.
    pub dt: PathBuf,
    /// UCI configuration file for the package.
    pub uci_config: PathBuf,
    /// Directory for the state snapshot, legacy policy record and wake marker.
    pub run_dir: PathBuf,
    /// Exclusive control lock directory.
    pub lock_dir: PathBuf,
    /// `/proc` equivalent, used only for liveness checks.
    pub proc_root: PathBuf,
    /// MT5700M manager temperature cache.
    pub module_cache: PathBuf,
    /// Maximum accepted age of the module cache, in seconds.
    pub module_cache_max_age: u64,
    /// Directory scanned for the legacy QModem style cache files.
    pub legacy_cache_dir: PathBuf,
    /// TCP port of the local at-webserver (mt5700) RPC bridge the module
    /// temperature is bridged from. 0 disables the bridge, which the
    /// sandboxed tests rely on.
    pub mt5700_rpc_port: u16,
    /// Log socket used for syslog (`/dev/log`).
    pub syslog_socket: PathBuf,
}

fn from_env(key: &str, default: &str) -> PathBuf {
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
        _ => PathBuf::from(default),
    }
}

fn uint_from_env(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn port_from_env(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u16>().ok())
        .unwrap_or(default)
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            sysfs: PathBuf::from("/sys"),
            radio: PathBuf::from("/sys/class/net"),
            dt: PathBuf::from("/sys/firmware/devicetree/base"),
            uci_config: PathBuf::from("/etc/config/h5000m_fancontrol"),
            run_dir: PathBuf::from("/var/run"),
            lock_dir: PathBuf::from("/var/lock/h5000m-fancontrol.lock"),
            proc_root: PathBuf::from("/proc"),
            module_cache: PathBuf::from("/var/run/mt5700m/temperature"),
            module_cache_max_age: 60,
            legacy_cache_dir: PathBuf::from("/tmp"),
            mt5700_rpc_port: port_from_env("H5000M_FAN_MT5700M_RPC_PORT", 8765),
            syslog_socket: PathBuf::from("/dev/log"),
        }
    }
}

impl Paths {
    pub fn from_env() -> Self {
        Paths {
            sysfs: from_env("H5000M_FAN_SENSOR_SYSFS_ROOT", "/sys"),
            radio: from_env("H5000M_FAN_RADIO_SYSFS_ROOT", "/sys/class/net"),
            dt: from_env("H5000M_FAN_DT_ROOT", "/sys/firmware/devicetree/base"),
            uci_config: from_env("H5000M_FAN_UCI_CONFIG", "/etc/config/h5000m_fancontrol"),
            run_dir: from_env("H5000M_FAN_RUN_DIR", "/var/run"),
            lock_dir: from_env("H5000M_FAN_LOCK_DIR", "/var/lock/h5000m-fancontrol.lock"),
            proc_root: from_env("H5000M_FAN_PROC_ROOT", "/proc"),
            module_cache: from_env(
                "H5000M_FAN_MT5700M_TEMP_CACHE",
                "/var/run/mt5700m/temperature",
            ),
            module_cache_max_age: uint_from_env("H5000M_FAN_MT5700M_TEMP_MAX_AGE", 60),
            legacy_cache_dir: from_env("H5000M_FAN_LEGACY_CACHE_DIR", "/tmp"),
            mt5700_rpc_port: port_from_env("H5000M_FAN_MT5700M_RPC_PORT", 8765),
            syslog_socket: from_env("H5000M_FAN_SYSLOG_SOCKET", "/dev/log"),
        }
    }

    /// Everything the controller reads lives under a single directory.
    /// Used by the regression tests; never points at real hardware.
    pub fn sandbox(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Paths {
            sysfs: root.join("sys"),
            radio: root.join("sys/class/net"),
            dt: root.join("sys/firmware/devicetree/base"),
            uci_config: root.join("etc/config/h5000m_fancontrol"),
            run_dir: root.join("var/run"),
            lock_dir: root.join("var/lock/h5000m-fancontrol.lock"),
            proc_root: root.join("proc"),
            module_cache: root.join("var/run/mt5700m/temperature"),
            module_cache_max_age: 60,
            legacy_cache_dir: root.join("tmp"),
            mt5700_rpc_port: 0,
            syslog_socket: root.join("dev/log"),
        }
    }

    pub fn thermal_dir(&self) -> PathBuf {
        self.sysfs.join("class/thermal")
    }

    pub fn hwmon_dir(&self) -> PathBuf {
        self.sysfs.join("class/hwmon")
    }

    pub fn state_file(&self) -> PathBuf {
        self.run_dir.join("h5000m-fancontrol.state")
    }

    /// Written by the 2.0.x shell controller when it switched a thermal zone to
    /// `user_space`. Only ever read to undo that, never created by 3.x.
    pub fn legacy_policy_file(&self) -> PathBuf {
        self.run_dir.join("h5000m-fancontrol.policy")
    }

    pub fn wake_file(&self) -> PathBuf {
        self.run_dir.join("h5000m-fancontrol.wake")
    }

    pub fn event_log(&self) -> PathBuf {
        self.run_dir.join("h5000m-fancontrol.events")
    }

    pub fn cooling_map_root(&self) -> PathBuf {
        self.dt.join("thermal-zones")
    }

    pub fn pwm_fan_dt(&self) -> PathBuf {
        self.dt.join("pwm-fan")
    }
}

/// Read a small file, trimming the trailing newline sysfs adds.
/// Returns `None` for unreadable files *and* for empty content so that callers
/// can treat both as "no reading" rather than as a zero value.
pub fn read_trimmed(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value = raw.trim_matches(|c| c == '\n' || c == '\r' || c == ' ' || c == '\t');
    if value.is_empty() {
        return None;
    }
    Some(value.to_string())
}

/// List directories whose name starts with `prefix`, naturally sorted.
pub fn list_dirs(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(prefix) {
                continue;
            }
            if entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    sort_by_trailing_index(&mut out);
    out
}

/// List files whose name starts with `prefix`, naturally sorted.
pub fn list_files(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) && entry.path().is_file() {
                out.push(entry.path());
            }
        }
    }
    sort_by_trailing_index(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_index_sort_is_numeric() {
        let mut paths = vec![
            PathBuf::from("/x/hwmon10"),
            PathBuf::from("/x/hwmon2"),
            PathBuf::from("/x/hwmon1"),
            PathBuf::from("/x/hwmon"),
        ];
        sort_by_trailing_index(&mut paths);
        assert_eq!(
            paths,
            vec![
                // A name without a trailing index sorts last: it is not a
                // numbered device and must not outrank one.
                PathBuf::from("/x/hwmon1"),
                PathBuf::from("/x/hwmon2"),
                PathBuf::from("/x/hwmon10"),
                PathBuf::from("/x/hwmon"),
            ]
        );
    }

    #[test]
    fn sandbox_paths_do_not_touch_real_sysfs() {
        let paths = Paths::sandbox("/tmp/does-not-exist");
        assert!(!paths.sysfs.starts_with("/sys"));
        assert!(!paths
            .lock_dir
            .starts_with("/var/lock/h5000m-fancontrol.lock"));
    }
}
