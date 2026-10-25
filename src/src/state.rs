//! Persisted controller state (`/var/run/h5000m-fancontrol.state`).
//!
//! The on-disk format keeps the historical `key=value` lines so anything that
//! already reads the file (scripts, the LuCI app before an upgrade) keeps
//! working. New keys are appended, never renamed.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControllerState {
    pub updated: Option<u64>,
    pub result: String,
    pub reason: String,
    pub requested_pwm: Option<u32>,
    pub target_pwm: Option<u32>,
    pub applied_pwm: Option<u32>,
    pub thermal_floor: Option<u32>,
    pub thermal_state: Option<u32>,
    pub thermal_max_state: Option<u32>,
    pub thermal_owner: String,
    pub control_temp: Option<i32>,
    pub control_sensor: String,
    pub down_since: Option<u64>,
    pub last_error: String,
}

impl ControllerState {
    pub fn from_text(text: &str) -> ControllerState {
        let mut map: HashMap<String, String> = HashMap::new();
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                map.entry(key.to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
        ControllerState {
            updated: map.get("updated").and_then(|v| v.parse().ok()),
            result: map.get("result").cloned().unwrap_or_default(),
            reason: map.get("reason").cloned().unwrap_or_default(),
            requested_pwm: map.get("requested_pwm").and_then(|v| v.parse().ok()),
            target_pwm: map.get("target_pwm").and_then(|v| v.parse().ok()),
            applied_pwm: map.get("applied_pwm").and_then(|v| v.parse().ok()),
            thermal_floor: map.get("thermal_floor").and_then(|v| v.parse().ok()),
            thermal_state: map.get("thermal_state").and_then(|v| v.parse().ok()),
            thermal_max_state: map.get("thermal_max_state").and_then(|v| v.parse().ok()),
            thermal_owner: map.get("thermal_owner").cloned().unwrap_or_default(),
            control_temp: map.get("control_temp").and_then(|v| v.parse().ok()),
            control_sensor: map.get("control_sensor").cloned().unwrap_or_default(),
            down_since: map.get("down_since").and_then(|v| v.parse().ok()),
            last_error: map.get("last_error").cloned().unwrap_or_default(),
        }
    }

    pub fn read(path: &Path) -> ControllerState {
        std::fs::read_to_string(path)
            .map(|text| ControllerState::from_text(&text))
            .unwrap_or_default()
    }

    pub fn to_text(&self) -> String {
        let opt_u64 = |v: Option<u64>| v.map(|n| n.to_string()).unwrap_or_default();
        let opt_u32 = |v: Option<u32>| v.map(|n| n.to_string()).unwrap_or_default();
        let opt_i32 = |v: Option<i32>| v.map(|n| n.to_string()).unwrap_or_default();
        format!(
            "updated={}\nresult={}\nreason={}\nrequested_pwm={}\ntarget_pwm={}\napplied_pwm={}\n\
             thermal_floor={}\nthermal_state={}\nthermal_max_state={}\nthermal_owner={}\n\
             control_temp={}\ncontrol_sensor={}\ndown_since={}\nlast_error={}\n",
            opt_u64(self.updated),
            self.result,
            self.reason,
            opt_u32(self.requested_pwm),
            opt_u32(self.target_pwm),
            opt_u32(self.applied_pwm),
            opt_u32(self.thermal_floor),
            opt_u32(self.thermal_state),
            opt_u32(self.thermal_max_state),
            self.thermal_owner,
            opt_i32(self.control_temp),
            self.control_sensor,
            opt_u64(self.down_since),
            self.last_error,
        )
    }

    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        atomic_write(path, &self.to_text())
    }
}

/// Write-via-temp-then-rename. On Linux the rename is atomic; the fallback
/// covers platforms (and tests) where renaming over an existing file fails.
pub fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            let result = std::fs::remove_file(path).and_then(|_| std::fs::rename(&tmp, path));
            if result.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_state_file() {
        let state = ControllerState {
            updated: Some(1700000000),
            result: "ok".into(),
            reason: "auto+kernel-floor".into(),
            requested_pwm: Some(160),
            target_pwm: Some(180),
            applied_pwm: Some(180),
            thermal_floor: Some(180),
            thermal_state: Some(1),
            thermal_max_state: Some(3),
            thermal_owner: "shared".into(),
            control_temp: Some(62),
            control_sensor: "soc_thermal".into(),
            down_since: Some(0),
            last_error: String::new(),
        };
        let text = state.to_text();
        assert!(text.starts_with("updated=1700000000\n"));
        assert!(text.contains("reason=auto+kernel-floor\n"));
        let parsed = ControllerState::from_text(&text);
        assert_eq!(parsed, state);
    }

    #[test]
    fn missing_values_stay_unknown() {
        let parsed = ControllerState::from_text("result=ok\ncontrol_temp=\n");
        assert_eq!(parsed.result, "ok");
        assert_eq!(parsed.control_temp, None);
        assert_eq!(parsed.control_sensor, "");
        assert_eq!(parsed.updated, None);
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let tmp = std::env::temp_dir().join("h5fan-atomic-test.state");
        std::fs::write(&tmp, "old").unwrap();
        atomic_write(&tmp, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "new");
        let _ = std::fs::remove_file(&tmp);
    }
}
