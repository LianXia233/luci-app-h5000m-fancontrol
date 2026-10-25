//! PWM and tachometer access for the `pwm-fan` device.

use crate::error::{Error, Result};
use crate::paths::{list_dirs, read_trimmed, Paths};
use std::path::{Path, PathBuf};

pub const PWM_MIN: u8 = 0;
pub const PWM_MAX: u8 = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanDevice {
    /// `.../pwm1` control node.
    pub pwm_path: PathBuf,
    /// hwmon directory holding the control node.
    pub hwmon_dir: PathBuf,
    /// `.../fanN_input`, when the board wires a tachometer.
    pub rpm_path: Option<PathBuf>,
    /// Matching `cooling_deviceN` for the kernel-side state.
    pub cooling_device: Option<CoolingDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoolingDevice {
    pub path: PathBuf,
    pub max_state: u32,
}

/// Locate the PWM control node.
///
/// A hwmon chip literally named `pwmfan` wins; only when no such chip exists
/// does the first other `pwm1` get used. That preference matters because some
/// vendor thermal chips also export a `pwm1` that is not the case fan.
pub fn discover(paths: &Paths) -> Option<FanDevice> {
    let mut fallback: Option<PathBuf> = None;
    let mut chosen: Option<PathBuf> = None;
    for hwmon in list_dirs(&paths.hwmon_dir(), "hwmon") {
        let pwm = hwmon.join("pwm1");
        if !pwm.exists() {
            continue;
        }
        let name = read_trimmed(&hwmon.join("name")).unwrap_or_default();
        if name == "pwmfan" {
            chosen = Some(pwm);
            break;
        }
        if fallback.is_none() {
            fallback = Some(pwm);
        }
    }
    let pwm_path = chosen.or(fallback)?;
    let hwmon_dir = pwm_path.parent()?.to_path_buf();
    Some(FanDevice {
        rpm_path: find_rpm_path(&hwmon_dir),
        cooling_device: cooling_device(paths),
        pwm_path,
        hwmon_dir,
    })
}

fn find_rpm_path(hwmon_dir: &Path) -> Option<PathBuf> {
    let parent = hwmon_dir.parent()?;
    for hwmon in list_dirs(parent, "hwmon") {
        if hwmon != *hwmon_dir {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&hwmon) else {
            continue;
        };
        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("fan") && name.ends_with("_input") && entry.path().is_file() {
                candidates.push(entry.path());
            }
        }
        crate::paths::sort_by_trailing_index(&mut candidates);
        return candidates.into_iter().next();
    }
    None
}

/// The kernel `pwm-fan` cooling device, if registered.
pub fn cooling_device(paths: &Paths) -> Option<CoolingDevice> {
    for device in list_dirs(&paths.thermal_dir(), "cooling_device") {
        match read_trimmed(&device.join("type")) {
            Some(kind) if kind == "pwm-fan" => {}
            _ => continue,
        }
        let max_state = read_trimmed(&device.join("max_state"))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        return Some(CoolingDevice {
            path: device,
            max_state,
        });
    }
    None
}

/// The `cpufreq` cooling device proves CPU throttling is still wired up.
pub fn cpufreq_cooling_device(paths: &Paths) -> Option<PathBuf> {
    list_dirs(&paths.thermal_dir(), "cooling_device")
        .into_iter()
        .find(|device| read_trimmed(&device.join("type")).as_deref() == Some("cpufreq"))
}

impl FanDevice {
    pub fn of_node(&self) -> Option<PathBuf> {
        // `<hwmon>/device/of_node` is a symlink into the flattened devicetree.
        let link = self.hwmon_dir.join("device/of_node");
        if let Ok(resolved) = std::fs::canonicalize(&link) {
            return Some(resolved);
        }
        let target = std::fs::read_link(&link).ok()?;
        if target.is_absolute() {
            return Some(target);
        }
        let parent = link.parent()?;
        Some(parent.join(target))
    }

    pub fn read_pwm(&self) -> Option<u8> {
        let raw = read_trimmed(&self.pwm_path)?;
        let value = raw.parse::<u32>().ok()?;
        Some(value.min(u32::from(PWM_MAX)) as u8)
    }

    /// Read the register without clamping, for the status report.
    pub fn read_pwm_raw(&self) -> Option<u32> {
        read_trimmed(&self.pwm_path).and_then(|raw| raw.parse::<u32>().ok())
    }

    pub fn read_rpm(&self) -> Option<u32> {
        let path = self.rpm_path.as_ref()?;
        read_trimmed(path).and_then(|raw| raw.parse::<u32>().ok())
    }

    /// Write one PWM value. The value is always read back so that a register
    /// silently clamped by the driver is reported as the real output.
    pub fn write_pwm(&self, value: u8) -> Result<u8> {
        std::fs::write(&self.pwm_path, format!("{value}\n")).map_err(|err| Error::PwmWrite {
            path: self.pwm_path.display().to_string(),
            message: err.to_string(),
        })?;
        Ok(self.read_pwm().unwrap_or(value))
    }

    pub fn pwm_max(&self) -> Option<u32> {
        read_trimmed(&self.hwmon_dir.join("pwm1_max")).and_then(|raw| raw.parse::<u32>().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpm_helper_is_not_needed_for_discovery_api_shape() {
        // Discovery against a non-existent root must return None instead of
        // panicking, because the daemon treats a missing fan as an error state.
        let paths = Paths::sandbox(std::env::temp_dir().join("h5fan-absent-root"));
        assert!(discover(&paths).is_none());
        assert!(cooling_device(&paths).is_none());
    }
}
