//! Filesystem layout of the controller.
//!
//! The production defaults match the historical shell controller exactly. The
//! environment overrides exist so tests and diagnostics can point every sysfs
//! lookup at an isolated fake tree instead of real hardware.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    /// Root for temperature sensor lookups (`/sys/class/thermal`,
    /// `/sys/class/hwmon`). Override: `H5000M_FAN_SENSOR_SYSFS_ROOT`.
    pub sensor_root: PathBuf,
    /// Root for PWM discovery, RPM inputs, cooling devices and thermal policy
    /// inspection. Override: `H5000M_FAN_PWM_SYSFS_ROOT`.
    pub pwm_root: PathBuf,
    /// Network interface directory used for vendor radio probing.
    /// Override: `H5000M_FAN_RADIO_SYSFS_ROOT`.
    pub radio_root: PathBuf,
    /// Device-tree base directory for cooling-map analysis.
    /// Override: `H5000M_FAN_DT_ROOT`.
    pub dt_root: PathBuf,
    /// Runtime directory holding the state file and daemon snapshot.
    /// Override: `H5000M_FAN_STATE_DIR`.
    pub state_dir: PathBuf,
    /// Lock directory guarding concurrent control decisions.
    /// Override: `H5000M_FAN_LOCK_DIR`.
    pub lock_dir: PathBuf,
    /// MT5700M manager temperature cache. Override:
    /// `H5000M_FAN_MT5700M_TEMP_CACHE`.
    pub module_cache: PathBuf,
    /// Maximum age (seconds) of the module cache before it counts as stale.
    /// Override: `H5000M_FAN_MT5700M_TEMP_MAX_AGE`.
    pub module_cache_max_age: u64,
    /// Directory scanned for legacy QModem cache files (`cache_base_info_*`,
    /// `cache_*info*`). Override: `H5000M_FAN_LEGACY_CACHE_DIR`.
    pub legacy_cache_dir: PathBuf,
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            sensor_root: PathBuf::from("/sys"),
            pwm_root: PathBuf::from("/sys"),
            radio_root: PathBuf::from("/sys/class/net"),
            dt_root: PathBuf::from("/sys/firmware/devicetree/base"),
            state_dir: PathBuf::from("/var/run"),
            lock_dir: PathBuf::from("/var/lock/h5000m-fancontrol.lock"),
            module_cache: PathBuf::from("/var/run/mt5700m/temperature"),
            module_cache_max_age: 60,
            legacy_cache_dir: PathBuf::from("/tmp"),
        }
    }
}

impl Paths {
    pub fn from_env() -> Self {
        Paths {
            sensor_root: env_path("H5000M_FAN_SENSOR_SYSFS_ROOT", "/sys"),
            pwm_root: env_path("H5000M_FAN_PWM_SYSFS_ROOT", "/sys"),
            radio_root: env_path("H5000M_FAN_RADIO_SYSFS_ROOT", "/sys/class/net"),
            dt_root: env_path("H5000M_FAN_DT_ROOT", "/sys/firmware/devicetree/base"),
            state_dir: env_path("H5000M_FAN_STATE_DIR", "/var/run"),
            lock_dir: env_path("H5000M_FAN_LOCK_DIR", "/var/lock/h5000m-fancontrol.lock"),
            module_cache: env_path(
                "H5000M_FAN_MT5700M_TEMP_CACHE",
                "/var/run/mt5700m/temperature",
            ),
            module_cache_max_age: env_u64("H5000M_FAN_MT5700M_TEMP_MAX_AGE", 60),
            legacy_cache_dir: env_path("H5000M_FAN_LEGACY_CACHE_DIR", "/tmp"),
        }
    }

    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("h5000m-fancontrol.state")
    }

    pub fn policy_file(&self) -> PathBuf {
        self.state_dir.join("h5000m-fancontrol.policy")
    }

    pub fn snapshot_file(&self) -> PathBuf {
        self.state_dir.join("h5000m-fancontrol.snapshot.json")
    }
}

/// Glob-free, sorted directory listing matching `prefix` + word characters,
/// mirroring POSIX shell glob expansion order (lexicographic by file name).
pub fn sorted_glob(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| {
                        n.len() > prefix.len()
                            && n.starts_with(prefix)
                            && n[prefix.len()..].bytes().all(|b| b.is_ascii_digit())
                    })
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// All direct children of `dir`, sorted by file name (shell glob order).
pub fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

pub fn read_trim(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_glob_matches_shell_order() {
        let tmp = std::env::temp_dir().join("h5fan-glob-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("thermal_zone2")).unwrap();
        std::fs::create_dir_all(tmp.join("thermal_zone10")).unwrap();
        std::fs::create_dir_all(tmp.join("thermal_zone1")).unwrap();
        std::fs::create_dir_all(tmp.join("other")).unwrap();
        let names: Vec<String> = sorted_glob(&tmp, "thermal_zone")
            .iter()
            .filter_map(|p| p.file_name().unwrap().to_str().map(String::from))
            .collect();
        // Shell glob order is plain lexicographic: 1, 10, 2.
        assert_eq!(
            names,
            vec!["thermal_zone1", "thermal_zone10", "thermal_zone2"]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
