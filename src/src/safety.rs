//! Kernel cooling-map analysis and the PWM safety floor.
//!
//! The kernel thermal governor may also drive the same PWM output through
//! `cooling-device` references in the device tree. Direct PWM writes change
//! `cur_state`, so the current cooling state alone is not a safe floor.
//! Instead, this module derives the minimum required cooling state from the
//! kernel's own cooling maps and translates it into a PWM floor that the
//! controller never undercuts. When the fan has no cooling maps (firmware
//! built with the exclusive-fan patch), conservative high-temperature floors
//! keep emergency cooling available.

use crate::paths::{read_trim, sorted_children, sorted_glob, Paths};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThermalOwner {
    /// No cooling device for the fan was found.
    Unknown,
    /// The kernel has cooling maps referencing the fan.
    Shared,
    /// No cooling map references the fan; userspace owns the fan policy.
    Userspace,
    /// The controller is disabled / kernel-only mode.
    Kernel,
    /// Userspace owns the PWM output while the kernel governor stays intact.
    UserspacePwm,
}

impl ThermalOwner {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThermalOwner::Unknown => "unknown",
            ThermalOwner::Shared => "shared",
            ThermalOwner::Userspace => "userspace",
            ThermalOwner::Kernel => "kernel",
            ThermalOwner::UserspacePwm => "userspace-pwm",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThermalFloor {
    pub floor: u32,
    pub state: u32,
    pub max_state: u32,
    pub owner: ThermalOwner,
}

impl Default for ThermalFloor {
    fn default() -> Self {
        ThermalFloor {
            floor: 0,
            state: 0,
            max_state: 0,
            owner: ThermalOwner::Unknown,
        }
    }
}

/// Reads the `index`-th big-endian u32 word of a binary device-tree property.
pub fn read_be32(path: &Path, index: usize) -> Option<u32> {
    let bytes = std::fs::read(path).ok()?;
    let start = index.checked_mul(4)?;
    let end = start.checked_add(4)?;
    if end > bytes.len() {
        return None;
    }
    Some(u32::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ]))
}

/// First `cooling_device*` of type `pwm-fan`.
fn cooling_device_path(pwm_root: &Path) -> Option<PathBuf> {
    sorted_glob(&pwm_root.join("class/thermal"), "cooling_device")
        .into_iter()
        .find(|dev| read_trim(&dev.join("type")).unwrap_or_default() == "pwm-fan")
}

/// PWM value of a cooling level from the `cooling-levels` DT property. The
/// property next to the PWM hwmon device takes precedence, then the standard
/// `pwm-fan` node.
fn cooling_level_for_state(pwm_path: &Path, dt_root: &Path, state: u32) -> Option<u32> {
    let hwmon_dir = pwm_path.parent()?;
    let mut levels = hwmon_dir
        .join("device/of_node/cooling-levels")
        .canonicalize()
        .ok();
    if !levels.as_ref().is_some_and(|p| p.is_file()) {
        levels = None;
    }
    let levels = match levels {
        Some(path) => path,
        None => {
            let fallback = dt_root.join("pwm-fan/cooling-levels");
            if fallback.is_file() {
                fallback
            } else {
                return None;
            }
        }
    };
    let value = read_be32(&levels, state as usize)?;
    if value <= 255 {
        Some(value)
    } else {
        None
    }
}

/// Computes the kernel-derived PWM safety floor for the current CPU
/// temperature. `cpu_temp` of `None` (sensor unknown) is handled fail-safe:
/// whenever the kernel owns or shares the fan, a missing CPU reading yields
/// the maximum floor.
pub fn thermal_floor(paths: &Paths, pwm_path: &Path, cpu_temp: Option<i32>) -> ThermalFloor {
    let mut result = ThermalFloor::default();
    let Some(dev) = cooling_device_path(&paths.pwm_root) else {
        return result;
    };
    result.max_state = read_trim(&dev.join("max_state"))
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    let hwmon_dir = pwm_path.parent().map(|p| p.to_path_buf());
    let dt_dir = hwmon_dir
        .as_ref()
        .and_then(|dir| dir.join("device/of_node").canonicalize().ok())
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| paths.dt_root.join("pwm-fan"));
    let fan_phandle = read_be32(&dt_dir.join("phandle"), 0);

    let zones_dir = paths.dt_root.join("thermal-zones");
    let mut maps_examined = false;
    let mut map_found = false;

    if let Some(fan_phandle) = fan_phandle {
        for zone in sorted_children(&zones_dir) {
            for map in sorted_children(&zone.join("cooling-maps")) {
                let cooling_device = map.join("cooling-device");
                let trip_ref_file = map.join("trip");
                if !cooling_device.is_file() || !trip_ref_file.is_file() {
                    continue;
                }
                let Some(map_phandle) = read_be32(&cooling_device, 0) else {
                    continue;
                };
                maps_examined = true;
                if map_phandle != fan_phandle {
                    continue;
                }
                map_found = true;
                let (Some(map_state), Some(trip_ref)) =
                    (read_be32(&cooling_device, 1), read_be32(&trip_ref_file, 0))
                else {
                    continue;
                };
                for trip_zone in sorted_children(&zones_dir) {
                    for trip in sorted_children(&trip_zone.join("trips")) {
                        let phandle_file = trip.join("phandle");
                        let temperature_file = trip.join("temperature");
                        if !phandle_file.is_file() || !temperature_file.is_file() {
                            continue;
                        }
                        if read_be32(&phandle_file, 0) != Some(trip_ref) {
                            continue;
                        }
                        let Some(raw_threshold) = read_be32(&temperature_file, 0) else {
                            continue;
                        };
                        let raw_hyst = read_be32(&trip.join("hysteresis"), 0).unwrap_or(0);
                        let threshold = raw_threshold / 1000;
                        let hyst = raw_hyst / 1000;
                        if let Some(temp) = cpu_temp {
                            if temp >= (threshold as i32) - (hyst as i32)
                                && map_state > result.state
                            {
                                result.state = map_state;
                            }
                        }
                    }
                }
            }
        }
    }

    if maps_examined && !map_found {
        // Exclusive userspace fan policy: keep only the high-temperature
        // emergency floors (the legacy 40°C active map blocked useful
        // low-speed control).
        result.owner = ThermalOwner::Userspace;
        match cpu_temp {
            None => {
                result.state = 3;
                result.floor = 255;
            }
            Some(temp) => {
                if temp >= 113 {
                    result.state = 3;
                } else if temp >= 83 {
                    result.state = 2;
                }
                if result.state > 0 {
                    result.floor = cooling_level_for_state(pwm_path, &paths.dt_root, result.state)
                        .unwrap_or(255);
                }
            }
        }
        return result;
    }

    if map_found {
        result.owner = ThermalOwner::Shared;
    }
    let Some(temp) = cpu_temp else {
        result.floor = 255;
        return result;
    };

    // Conservative fallback for old H5000M firmware when the DT provides no
    // usable map data for the current temperature.
    if result.state == 0 {
        if temp >= 38 {
            result.state = 1;
        }
        if temp >= 83 {
            result.state = 2;
        }
        if temp >= 113 {
            result.state = 3;
        }
    }
    if result.state > 0 {
        result.floor =
            cooling_level_for_state(pwm_path, &paths.dt_root, result.state).unwrap_or(255);
    }
    result
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // test fixtures patch a few paths
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn write_be32(path: &Path, words: &[u32]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
        std::fs::write(path, bytes).unwrap();
    }

    /// Builds a fake tree: pwm-fan with cooling-levels [0,96,160,235], a
    /// pwm-fan cooling device, and a cpu thermal zone whose map references
    /// the fan for trips at 83°C/113°C.
    fn fixture(tmp: &Path) -> Paths {
        let _ = std::fs::remove_dir_all(tmp);
        let mut paths = Paths::default();
        paths.pwm_root = tmp.join("sys");
        paths.dt_root = tmp.join("dt");
        let hwmon = tmp.join("sys/class/hwmon/hwmon0");
        write(&hwmon.join("name"), "pwmfan\n");
        write(&hwmon.join("pwm1"), "0\n");
        let cooler = tmp.join("sys/class/thermal/cooling_device0");
        write(&cooler.join("type"), "pwm-fan\n");
        write(&cooler.join("max_state"), "3\n");
        let fan_node = tmp.join("dt/pwm-fan");
        write_be32(&fan_node.join("phandle"), &[0x2a]);
        write_be32(&fan_node.join("cooling-levels"), &[0, 96, 160, 235]);
        let zone = tmp.join("dt/thermal-zones/cpu-thermal");
        write_be32(&zone.join("trips/trip-hot/phandle"), &[0x11]);
        write_be32(&zone.join("trips/trip-hot/temperature"), &[113000]);
        write_be32(&zone.join("trips/trip-hot/hysteresis"), &[2000]);
        write_be32(&zone.join("trips/trip-crit/phandle"), &[0x12]);
        write_be32(&zone.join("trips/trip-crit/temperature"), &[120000]);
        write_be32(&zone.join("cooling-maps/map0/cooling-device"), &[0x2a, 2]);
        write_be32(&zone.join("cooling-maps/map0/trip"), &[0x11]);
        paths
    }

    fn pwm(paths: &Paths) -> PathBuf {
        paths.pwm_root.join("class/hwmon/hwmon0/pwm1")
    }

    #[test]
    fn shared_owner_uses_kernel_map_thresholds() {
        let tmp = std::env::temp_dir().join("h5fan-floor-shared");
        let paths = fixture(&tmp);
        let pwm = pwm(&paths);
        // 60°C is below every map trip, but the conservative fallback still
        // pins cooling state 1 (PWM 96) as the floor.
        let r = thermal_floor(&paths, &pwm, Some(60));
        assert_eq!(r.owner, ThermalOwner::Shared);
        assert_eq!(r.state, 1);
        assert_eq!(r.floor, 96);
        assert_eq!(r.max_state, 3);
        // Hysteresis (2°C) pulls the 113°C trip down to 111°C.
        let r = thermal_floor(&paths, &pwm, Some(111));
        assert_eq!(r.state, 2);
        assert_eq!(r.floor, 160);
        // Missing CPU sensor with shared ownership -> maximum floor.
        let r = thermal_floor(&paths, &pwm, None);
        assert_eq!(r.floor, 255);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn userspace_owner_keeps_emergency_floors_only() {
        let tmp = std::env::temp_dir().join("h5fan-floor-exclusive");
        let paths = fixture(&tmp);
        // Remove the fan from the cooling map -> maps examined, none matched.
        let map = tmp.join("dt/thermal-zones/cpu-thermal/cooling-maps/map0/cooling-device");
        write_be32(&map, &[0x99, 2]);
        let pwm = pwm(&paths);
        let r = thermal_floor(&paths, &pwm, Some(70));
        assert_eq!(r.owner, ThermalOwner::Userspace);
        assert_eq!(r.state, 0);
        assert_eq!(r.floor, 0);
        let r = thermal_floor(&paths, &pwm, Some(85));
        assert_eq!(r.state, 2);
        assert_eq!(r.floor, 160);
        let r = thermal_floor(&paths, &pwm, Some(115));
        assert_eq!(r.state, 3);
        assert_eq!(r.floor, 235);
        let r = thermal_floor(&paths, &pwm, None);
        assert_eq!(r.state, 3);
        assert_eq!(r.floor, 255);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fallback_thresholds_when_dt_maps_missing() {
        let tmp = std::env::temp_dir().join("h5fan-floor-fallback");
        let paths = fixture(&tmp);
        // Empty the fan phandle so no map is examined.
        let fan_node = tmp.join("dt/pwm-fan");
        write_be32(&fan_node.join("phandle"), &[0x01]);
        let map_dir = tmp.join("dt/thermal-zones/cpu-thermal/cooling-maps/map0");
        let _ = std::fs::remove_dir_all(&map_dir);
        let pwm = pwm(&paths);
        // Owner stays unknown but the conservative 38/83/113 floors apply.
        let r = thermal_floor(&paths, &pwm, Some(40));
        assert_eq!(r.owner, ThermalOwner::Unknown);
        assert_eq!(r.state, 1);
        assert_eq!(r.floor, 96);
        let r = thermal_floor(&paths, &pwm, Some(90));
        assert_eq!(r.state, 2);
        let r = thermal_floor(&paths, &pwm, Some(120));
        assert_eq!(r.state, 3);
        assert_eq!(r.floor, 235);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_cooling_levels_defaults_to_full_floor() {
        let tmp = std::env::temp_dir().join("h5fan-floor-nolevels");
        let paths = fixture(&tmp);
        std::fs::remove_file(tmp.join("dt/pwm-fan/cooling-levels")).unwrap();
        let pwm = pwm(&paths);
        let map = tmp.join("dt/thermal-zones/cpu-thermal/cooling-maps/map0/cooling-device");
        write_be32(&map, &[0x99, 2]);
        let r = thermal_floor(&paths, &pwm, Some(90));
        assert_eq!(r.owner, ThermalOwner::Userspace);
        assert_eq!(r.state, 2);
        assert_eq!(r.floor, 255);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn be32_reader_respects_word_index() {
        let tmp = std::env::temp_dir().join("h5fan-be32");
        let _ = std::fs::remove_dir_all(&tmp);
        let file = tmp.join("words");
        write_be32(&file, &[0x01020304, 0x0a0b0c0d]);
        assert_eq!(read_be32(&file, 0), Some(0x01020304));
        assert_eq!(read_be32(&file, 1), Some(0x0a0b0c0d));
        assert_eq!(read_be32(&file, 2), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
