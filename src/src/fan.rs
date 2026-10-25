//! PWM output discovery and access.

use crate::paths::{read_trim, sorted_children, sorted_glob};
use std::path::{Path, PathBuf};

/// Finds the PWM control node: prefers the hwmon chip named `pwmfan`, then
/// falls back to the first `pwm1` of any hwmon chip (shell glob order).
pub fn find_pwm(pwm_root: &Path) -> Option<PathBuf> {
    let hwmons = sorted_glob(&pwm_root.join("class/hwmon"), "hwmon");
    for hwmon in &hwmons {
        let name = read_trim(&hwmon.join("name")).unwrap_or_default();
        if name == "pwmfan" {
            let pwm = hwmon.join("pwm1");
            if pwm.exists() {
                return Some(pwm);
            }
        }
    }
    for hwmon in &hwmons {
        let pwm = hwmon.join("pwm1");
        if pwm.exists() {
            return Some(pwm);
        }
    }
    None
}

/// First `fan*_input` node across hwmon chips (shell glob order).
pub fn find_fan_rpm(pwm_root: &Path) -> Option<PathBuf> {
    for hwmon in sorted_glob(&pwm_root.join("class/hwmon"), "hwmon") {
        for fan in sorted_children(&hwmon) {
            let is_input = fan
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| {
                    n.starts_with("fan") && n.ends_with("_input") && n.len() > "fan_input".len()
                })
                .unwrap_or(false);
            if is_input && fan.exists() {
                return Some(fan);
            }
        }
    }
    None
}

pub fn read_pwm(path: &Path) -> Option<u32> {
    let raw = read_trim(path)?;
    raw.parse::<u32>().ok()
}

pub fn write_pwm(path: &Path, value: u32) -> std::io::Result<()> {
    std::fs::write(path, value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn prefers_named_pwmfan_then_first_pwm1() {
        let tmp = std::env::temp_dir().join("h5fan-pwm-test");
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp.join("sys/class/hwmon/hwmon0/name"), "something\n");
        write(&tmp.join("sys/class/hwmon/hwmon0/pwm1"), "100\n");
        write(&tmp.join("sys/class/hwmon/hwmon2/name"), "pwmfan\n");
        write(&tmp.join("sys/class/hwmon/hwmon2/pwm1"), "160\n");
        let root = tmp.join("sys");
        assert_eq!(find_pwm(&root), Some(root.join("class/hwmon/hwmon2/pwm1")));
        std::fs::remove_file(root.join("class/hwmon/hwmon2/name")).unwrap();
        assert_eq!(find_pwm(&root), Some(root.join("class/hwmon/hwmon0/pwm1")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn finds_fan_input_and_reads_pwm() {
        let tmp = std::env::temp_dir().join("h5fan-rpm-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let hwmon = tmp.join("sys/class/hwmon/hwmon0");
        write(&hwmon.join("name"), "pwmfan\n");
        write(&hwmon.join("pwm1"), "200\n");
        write(&hwmon.join("fan1_input"), "3200\n");
        let root = tmp.join("sys");
        assert_eq!(find_fan_rpm(&root), Some(hwmon.join("fan1_input")));
        assert_eq!(read_pwm(&hwmon.join("pwm1")), Some(200));
        write(&hwmon.join("pwm1"), "garbage");
        assert_eq!(read_pwm(&hwmon.join("pwm1")), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
