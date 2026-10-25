//! CPU thermal zone inspection and legacy policy recovery.
//!
//! The controller never switches a thermal zone to `user_space`. It only
//! inspects the CPU zone governor to report whether kernel throttling is
//! intact, and it repairs the leftover policy file written by pre-2.1.0
//! releases that did take the zone over.

use crate::paths::{read_trim, sorted_glob, Paths};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ZoneInfo {
    pub path: PathBuf,
    pub zone_type: String,
    pub policy: String,
}

/// Finds the CPU-ish thermal zone exposing `user_space` in its available
/// policies. Zones are examined in shell glob order; the first CPU/soc/mt798
/// typed zone wins, otherwise the first candidate is used as fallback.
pub fn find_thermal_zone(pwm_root: &Path) -> Option<ZoneInfo> {
    let mut fallback: Option<ZoneInfo> = None;
    for zone in sorted_glob(&pwm_root.join("class/thermal"), "thermal_zone") {
        let policy_path = zone.join("policy");
        let available_path = zone.join("available_policies");
        if !policy_path.exists() || !available_path.exists() {
            continue;
        }
        let available = read_trim(&available_path).unwrap_or_default();
        if !word_listed(&available, "user_space") {
            continue;
        }
        let zone_type = read_trim(&zone.join("type")).unwrap_or_default();
        let info = ZoneInfo {
            path: zone.clone(),
            zone_type: zone_type.clone(),
            policy: read_trim(&policy_path).unwrap_or_default(),
        };
        let lower = zone_type.to_lowercase();
        if lower.contains("cpu") || lower.contains("soc") || lower.contains("mt798") {
            return Some(info);
        }
        if fallback.is_none() {
            fallback = Some(info);
        }
    }
    fallback
}

/// Space-delimited word membership, mirroring the shell
/// `case " $list " in *" $word "*)` idiom.
fn word_listed(list: &str, word: &str) -> bool {
    list.split_whitespace().any(|token| token == word)
}

/// Restores the governor recorded in the policy file, written by legacy
/// releases that switched the whole CPU zone to `user_space`. Keeps the
/// recorded policy when it is still available, falls back to `step_wise`,
/// and never removes kernel CPU cooling.
pub fn release_thermal_control(paths: &Paths) {
    let policy_file = paths.policy_file();
    let Ok(text) = std::fs::read_to_string(&policy_file) else {
        return;
    };
    let mut lines = text.lines();
    let zone = lines.next().unwrap_or("").to_string();
    let mut policy = lines.next().unwrap_or("").to_string();
    let policy_path = Path::new(&zone).join("policy");
    if policy_path.exists() {
        let available = read_trim(&Path::new(&zone).join("available_policies")).unwrap_or_default();
        if !word_listed(&available, &policy) {
            if word_listed(&available, "step_wise") {
                policy = "step_wise".to_string();
            } else {
                policy.clear();
            }
        }
        if !policy.is_empty() {
            let _ = std::fs::write(&policy_path, &policy);
        }
    }
    let _ = std::fs::remove_file(&policy_file);
}

/// Result of inspecting the CPU thermal zone during a control cycle.
#[derive(Debug, Clone, Default)]
pub struct ThermalControlState {
    pub zone: Option<ZoneInfo>,
}

/// Inspects the kernel thermal governor. Fails only when the CPU zone exists
/// but its policy cannot be read - mirroring the shell controller, which
/// refuses to change cooling policy in that situation.
pub fn take_thermal_control(paths: &Paths) -> Result<ThermalControlState, String> {
    // One-shot repair of the policy file left behind by legacy releases.
    if paths.policy_file().exists() {
        release_thermal_control(paths);
    }
    match find_thermal_zone(&paths.pwm_root) {
        None => Ok(ThermalControlState { zone: None }),
        Some(zone) => {
            if zone.policy.is_empty() {
                Err("thermal zone policy unreadable".to_string())
            } else {
                Ok(ThermalControlState { zone: Some(zone) })
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // test fixtures patch a few paths
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn fixture(tmp: &Path) -> Paths {
        let mut paths = Paths::default();
        paths.pwm_root = tmp.join("sys");
        paths.state_dir = tmp.join("run");
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        paths
    }

    #[test]
    fn prefers_cpu_zone_over_fallback() {
        let tmp = std::env::temp_dir().join("h5fan-thermal-test-1");
        let _ = std::fs::remove_dir_all(&tmp);
        let z0 = tmp.join("sys/class/thermal/thermal_zone0");
        write(&z0.join("type"), "gpu\n");
        write(&z0.join("policy"), "step_wise\n");
        write(&z0.join("available_policies"), "step_wise user_space\n");
        let z1 = tmp.join("sys/class/thermal/thermal_zone1");
        write(&z1.join("type"), "cpu-thermal\n");
        write(&z1.join("policy"), "step_wise\n");
        write(
            &z1.join("available_policies"),
            "step_wise user_space fair_share\n",
        );
        let paths = fixture(&tmp);
        let zone = find_thermal_zone(&paths.pwm_root).unwrap();
        assert_eq!(zone.zone_type, "cpu-thermal");
        // Without the CPU zone the gpu zone serves as fallback.
        let _ = std::fs::remove_dir_all(&z1);
        let zone = find_thermal_zone(&paths.pwm_root).unwrap();
        assert_eq!(zone.zone_type, "gpu");
        // Zones without user_space support are ignored entirely.
        write(&z0.join("available_policies"), "step_wise\n");
        assert!(find_thermal_zone(&paths.pwm_root).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn policy_file_restores_recorded_governor() {
        let tmp = std::env::temp_dir().join("h5fan-thermal-test-2");
        let _ = std::fs::remove_dir_all(&tmp);
        let zone = tmp.join("sys/class/thermal/thermal_zone0");
        write(&zone.join("type"), "cpu-thermal\n");
        write(&zone.join("policy"), "user_space\n");
        write(&zone.join("available_policies"), "step_wise user_space\n");
        let paths = fixture(&tmp);
        std::fs::write(
            paths.policy_file(),
            format!("{}\nstep_wise\n", zone.display()),
        )
        .unwrap();
        release_thermal_control(&paths);
        assert_eq!(read_trim(&zone.join("policy")).unwrap(), "step_wise");
        assert!(!paths.policy_file().exists());
        // When neither the recorded governor nor step_wise is available the
        // kernel policy is left untouched.
        write(
            &zone.join("available_policies"),
            "power_allocator user_space\n",
        );
        std::fs::write(
            paths.policy_file(),
            format!("{}\nfair_share\n", zone.display()),
        )
        .unwrap();
        release_thermal_control(&paths);
        assert_eq!(read_trim(&zone.join("policy")).unwrap(), "step_wise");
        assert!(!paths.policy_file().exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn take_fails_when_policy_unreadable() {
        let tmp = std::env::temp_dir().join("h5fan-thermal-test-3");
        let _ = std::fs::remove_dir_all(&tmp);
        let zone = tmp.join("sys/class/thermal/thermal_zone0");
        write(&zone.join("type"), "cpu-thermal\n");
        write(&zone.join("policy"), "\n");
        write(&zone.join("available_policies"), "step_wise user_space\n");
        let paths = fixture(&tmp);
        // find_thermal_zone reads a whitespace-only policy as empty string.
        let state = take_thermal_control(&paths);
        assert!(state.is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
