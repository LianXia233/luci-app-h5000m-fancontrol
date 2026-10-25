//! Unified status model with two renderings:
//!
//! * JSON (`status --json`): the structured interface consumed by the LuCI
//!   frontend. When the daemon is healthy it is served from the snapshot the
//!   daemon refreshes every control interval, decoupling web poll frequency
//!   from sensor sampling; a stale or missing snapshot falls back to one
//!   bounded live read.
//! * Text (`status`): the historical `key=value` output, byte-compatible with
//!   the shell controller for external scripts.
//!
//! Missing measurements are `null` / empty - never zero.

use crate::config::Config;
use crate::curve;
use crate::fan;
use crate::jsonw::ObjectWriter;
use crate::paths::{read_trim, Paths};
use crate::safety::{thermal_floor, ThermalOwner};
use crate::sensor::{SensorReader, SensorValue, VendorRadio};
use crate::service::{daemon_running, now_epoch};
use crate::state::{atomic_write, ControllerState};
use crate::thermal;

pub struct StatusModel {
    pub generated: u64,
    pub source: &'static str,
    pub cfg: Config,
    pub running: bool,
    pub state: ControllerState,
    pub state_age: Option<u64>,
    pub sensors: crate::sensor::SensorSnapshot,
    pub pwm_path: Option<std::path::PathBuf>,
    pub pwm_value: Option<u32>,
    pub rpm_path: Option<std::path::PathBuf>,
    pub rpm: Option<u32>,
    pub floor_pwm: u32,
    pub floor_state: u32,
    pub floor_max_state: u32,
    pub owner: ThermalOwner,
    pub kernel_policy: Option<String>,
    pub kernel_zone_type: Option<String>,
}

pub fn build_live(
    paths: &Paths,
    cfg: &Config,
    vendor: &mut dyn VendorRadio,
    now: u64,
) -> StatusModel {
    let state = ControllerState::read(&paths.state_file());
    let sensors = SensorReader::new(paths).read(cfg.temp_source, vendor, now);
    let pwm_path = fan::find_pwm(&paths.pwm_root);
    let pwm_value = pwm_path.as_ref().and_then(|p| fan::read_pwm(p));
    let rpm_path = fan::find_fan_rpm(&paths.pwm_root);
    let rpm = rpm_path.as_ref().and_then(|p| read_rpm(p));
    let (floor_pwm, floor_state, floor_max_state, owner) = match &pwm_path {
        Some(pwm) => {
            let floor = thermal_floor(paths, pwm, sensors.cpu.temp);
            (floor.floor, floor.state, floor.max_state, floor.owner)
        }
        None => (0, 0, 0, ThermalOwner::Unknown),
    };
    let zone = thermal::find_thermal_zone(&paths.pwm_root);
    let kernel_policy = zone.as_ref().map(|z| z.policy.clone());
    let kernel_zone_type = zone.as_ref().map(|z| z.zone_type.clone());
    let mut owner = owner;
    if !cfg.enabled || cfg.mode == crate::config::Mode::Kernel {
        owner = ThermalOwner::Kernel;
    }
    let running = daemon_running(paths);
    let state_age = state.updated.map(|updated| now.saturating_sub(updated));
    StatusModel {
        generated: now,
        source: "live",
        running,
        state,
        state_age,
        sensors,
        pwm_path,
        pwm_value,
        rpm_path,
        rpm,
        floor_pwm,
        floor_state,
        floor_max_state,
        owner,
        kernel_policy,
        kernel_zone_type,
        cfg: cfg.clone(),
    }
}

fn read_rpm(path: &std::path::Path) -> Option<u32> {
    read_trim(path).and_then(|v| v.parse().ok())
}

impl StatusModel {
    /// "ok" | "warn" | "fail" | "unknown", mirroring and extending the old
    /// frontend health derivation (failsafe -> fail, stale state -> warn).
    pub fn health(&self) -> &'static str {
        match self.state.result.as_str() {
            "failsafe" | "error" => "fail",
            "" => "unknown",
            _ => {
                let threshold = self.cfg.interval as u64 * 3 + 5;
                match self.state_age {
                    None => "unknown",
                    Some(age) if age > threshold => "warn",
                    Some(_) => "ok",
                }
            }
        }
    }

    fn sensor_json(value: &SensorValue) -> String {
        let mut w = ObjectWriter::new();
        w.opt_int("temp_c", value.temp.map(|t| t as i64))
            .str("label", &value.label)
            .str("source", value.source.as_str());
        w.finish()
    }

    pub fn to_json(&self) -> String {
        let mut controller = ObjectWriter::new();
        controller
            .str("version", crate::VERSION)
            .bool("enabled", self.cfg.enabled)
            .str("mode", mode_str(&self.cfg))
            .str("curve", profile_str(&self.cfg))
            .str(
                "temp_source",
                if self.cfg.temp_source == crate::config::TempSource::Max {
                    "max"
                } else {
                    "cpu"
                },
            )
            .int("manual_pwm", self.cfg.manual_pwm as i64)
            .int("interval", self.cfg.interval as i64)
            .int("hysteresis", self.cfg.hysteresis as i64)
            .int("down_delay", self.cfg.down_delay as i64)
            .int("start_pwm", self.cfg.start_pwm as i64)
            .int("start_boost_ms", self.cfg.start_boost_ms as i64)
            .int("fail_pwm", self.cfg.fail_pwm as i64)
            .str("curve_data", self.cfg.curve_data());
        let points_json = curve::parse_curve(self.cfg.curve_data())
            .map(|points| {
                points
                    .iter()
                    .map(|p| {
                        let mut w = ObjectWriter::new();
                        w.int("temp", p.temp as i64).int("pwm", p.percent as i64);
                        w.finish()
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .map(|list| format!("[{list}]"))
            .unwrap_or_else(|| "null".to_string());
        controller
            .raw("curve_points", points_json)
            .bool("running", self.running)
            .str("health", self.health())
            .str("result", &self.state.result)
            .str("reason", &self.state.reason)
            .opt_int("last_success", self.state.updated.map(|v| v as i64))
            .str("last_error", &self.state.last_error)
            .opt_int("state_age", self.state_age.map(|v| v as i64));

        let mut sensors = ObjectWriter::new();
        sensors
            .raw("cpu", Self::sensor_json(&self.sensors.cpu))
            .raw("phy", Self::sensor_json(&self.sensors.phy))
            .raw("wifi1", Self::sensor_json(&self.sensors.wifi1))
            .raw("wifi2", Self::sensor_json(&self.sensors.wifi2))
            .raw("module", Self::sensor_json(&self.sensors.module));
        let control_json = match &self.sensors.control {
            Some(control) => {
                let mut w = ObjectWriter::new();
                w.opt_int("temp_c", control.temp.map(|t| t as i64))
                    .str("label", &control.label);
                w.finish()
            }
            None => "null".to_string(),
        };
        sensors.raw("control", control_json);

        let mut fan_obj = ObjectWriter::new();
        fan_obj
            .opt_str("pwm_path", self.pwm_path.as_ref().and_then(|p| p.to_str()))
            .opt_int("pwm", self.pwm_value.map(|v| v as i64))
            .opt_int(
                "pwm_percent",
                self.pwm_value.map(|v| (v as u64 * 100 / 255) as i64),
            )
            .opt_int("requested_pwm", self.state.requested_pwm.map(|v| v as i64))
            .opt_int("target_pwm", self.state.target_pwm.map(|v| v as i64))
            .opt_int("applied_pwm", self.state.applied_pwm.map(|v| v as i64))
            .opt_int("rpm", self.rpm.map(|v| v as i64))
            .opt_str("rpm_path", self.rpm_path.as_ref().and_then(|p| p.to_str()))
            .bool("rpm_supported", self.rpm_path.is_some());

        let mut safety = ObjectWriter::new();
        safety
            .int("thermal_floor", self.floor_pwm as i64)
            .int("thermal_state", self.floor_state as i64)
            .int("thermal_max_state", self.floor_max_state as i64)
            .str("thermal_owner", self.owner.as_str())
            .opt_str("kernel_policy", self.kernel_policy.as_deref())
            .opt_str("kernel_zone", self.kernel_zone_type.as_deref())
            .bool(
                "cpu_throttling_intact",
                match &self.kernel_policy {
                    None => true,
                    Some(policy) => policy != "user_space",
                },
            )
            .bool("down_delay_active", self.state.reason == "auto-down-delay");

        let mut root = ObjectWriter::new();
        root.int("generated", self.generated as i64)
            .str("source", self.source)
            .raw("controller", controller.finish())
            .raw("sensors", sensors.finish())
            .raw("fan", fan_obj.finish())
            .raw("safety", safety.finish());
        root.finish()
    }

    /// Historical `key=value` output, field-for-field compatible with the
    /// shell controller's `print_status`.
    pub fn to_legacy_text(&self) -> String {
        let cfg = &self.cfg;
        let mut lines: Vec<String> = Vec::new();
        let mut push = |k: &str, v: String| lines.push(format!("{k}={v}"));
        push("enabled", if cfg.enabled { "1".into() } else { "0".into() });
        push("mode", mode_str(cfg).into());
        push("curve", profile_str(cfg).into());
        push("curve_data", cfg.curve_data().into());
        push(
            "temp_source",
            if cfg.temp_source == crate::config::TempSource::Max {
                "max".into()
            } else {
                "cpu".into()
            },
        );
        push("manual_pwm", cfg.manual_pwm.to_string());
        push("interval", cfg.interval.to_string());
        push("hysteresis", cfg.hysteresis.to_string());
        push("down_delay", cfg.down_delay.to_string());
        push("start_pwm", cfg.start_pwm.to_string());
        push("start_boost_ms", cfg.start_boost_ms.to_string());
        push(
            "pwm",
            self.pwm_path
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string(),
        );
        push(
            "pwm_value",
            self.pwm_value.map(|v| v.to_string()).unwrap_or_default(),
        );
        push(
            "fan",
            self.rpm_path
                .as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string(),
        );
        if self.rpm_path.is_some() {
            push("fan_feedback", "1".into());
            push(
                "fan_rpm",
                self.rpm.map(|v| v.to_string()).unwrap_or_default(),
            );
        } else {
            push("fan_feedback", "0".into());
            push("fan_rpm", String::new());
        }
        push(
            "cpu_temp",
            self.sensors
                .cpu
                .temp
                .map(|t| t.to_string())
                .unwrap_or_default(),
        );
        push("cpu_label", self.sensors.cpu.label.clone());
        push(
            "phy_temp",
            self.sensors
                .phy
                .temp
                .map(|t| t.to_string())
                .unwrap_or_default(),
        );
        push("phy_label", self.sensors.phy.label.clone());
        push(
            "wifi1_temp",
            self.sensors
                .wifi1
                .temp
                .map(|t| t.to_string())
                .unwrap_or_default(),
        );
        push("wifi1_label", self.sensors.wifi1.label.clone());
        push(
            "wifi2_temp",
            self.sensors
                .wifi2
                .temp
                .map(|t| t.to_string())
                .unwrap_or_default(),
        );
        push("wifi2_label", self.sensors.wifi2.label.clone());
        push(
            "module_temp",
            self.sensors
                .module
                .temp
                .map(|t| t.to_string())
                .unwrap_or_default(),
        );
        push("module_label", "5G modem".into());
        push(
            "control_temp",
            self.sensors
                .control
                .as_ref()
                .and_then(|c| c.temp)
                .map(|t| t.to_string())
                .unwrap_or_default(),
        );
        push(
            "control_sensor",
            self.sensors
                .control
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_default(),
        );
        push("thermal_floor", self.floor_pwm.to_string());
        push("thermal_state", self.floor_state.to_string());
        push("thermal_max_state", self.floor_max_state.to_string());
        push("thermal_owner", self.owner.as_str().into());
        push("result", self.state.result.clone());
        push("reason", self.state.reason.clone());
        push(
            "requested_pwm",
            self.state
                .requested_pwm
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
        push(
            "target_pwm",
            self.state
                .target_pwm
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
        push(
            "applied_pwm",
            self.state
                .applied_pwm
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
        push(
            "state_age",
            self.state_age.map(|v| v.to_string()).unwrap_or_default(),
        );
        lines.join("\n") + "\n"
    }
}

fn mode_str(cfg: &Config) -> &'static str {
    match cfg.mode {
        crate::config::Mode::Auto => "auto",
        crate::config::Mode::Manual => "manual",
        crate::config::Mode::Kernel => "kernel",
    }
}

fn profile_str(cfg: &Config) -> &'static str {
    match cfg.profile {
        crate::config::Profile::Silent => "silent",
        crate::config::Profile::Balanced => "balanced",
        crate::config::Profile::Performance => "performance",
        crate::config::Profile::Custom => "custom",
    }
}

/// Serves the daemon snapshot when it is fresh enough, else computes live.
/// Freshness: twice the control interval (minimum 5 s) - short enough that a
/// dead daemon is noticed quickly, long enough that normal polling never
/// triggers hardware queries itself.
pub fn json_status(paths: &Paths, cfg: &Config, vendor: &mut dyn VendorRadio, now: u64) -> String {
    let max_age = (cfg.interval as u64 * 2).max(5);
    if let Ok(text) = std::fs::read_to_string(paths.snapshot_file()) {
        if let Some(age) = snapshot_age(&text, now) {
            if age <= max_age {
                return text;
            }
        }
    }
    build_live(paths, cfg, vendor, now).to_json()
}

fn snapshot_age(text: &str, now: u64) -> Option<u64> {
    let marker = "\"generated\":";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find(',')?;
    now.checked_sub(rest[..end].trim().parse::<u64>().ok()?)
}

/// Daemon-side snapshot refresh after each successful control cycle.
pub fn write_snapshot(paths: &Paths, cfg: &Config) -> Result<(), String> {
    let mut vendor = crate::sensor::IwprivVendor::new(paths.radio_root.clone());
    // The snapshot reuses the sensor readings already computed during the
    // cycle would be ideal; re-reading is cheap and keeps this function
    // independent (hwmon reads are a few sysfs file opens).
    let model = build_live(paths, cfg, &mut vendor, now_epoch());
    atomic_write(&paths.snapshot_file(), &model.to_json())
        .map_err(|e| format!("snapshot write failed: {e}"))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // test fixtures patch a few paths
mod tests {
    use super::*;
    use crate::sensor::NoVendor;
    use crate::uci::MapSource;
    use std::collections::HashMap;

    fn write(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn fixture(tmp: &std::path::Path) -> Paths {
        let _ = std::fs::remove_dir_all(tmp);
        let mut paths = Paths::default();
        paths.pwm_root = tmp.join("sys");
        paths.sensor_root = tmp.join("sys");
        paths.dt_root = tmp.join("dt");
        paths.state_dir = tmp.join("run");
        paths.lock_dir = tmp.join("lock/h5000m-fancontrol.lock");
        paths.module_cache = tmp.join("none/temperature");
        paths.legacy_cache_dir = tmp.join("none-tmp");
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        let hwmon = tmp.join("sys/class/hwmon/hwmon0");
        write(&hwmon.join("name"), "pwmfan\n");
        write(&hwmon.join("pwm1"), "160\n");
        write(&hwmon.join("fan1_input"), "3100\n");
        let zone = tmp.join("sys/class/thermal/thermal_zone0");
        write(&zone.join("type"), "soc_thermal\n");
        write(&zone.join("temp"), "63000\n");
        write(&zone.join("policy"), "step_wise\n");
        write(&zone.join("available_policies"), "step_wise user_space\n");
        paths
    }

    #[test]
    fn json_contains_all_sections_and_missing_temps_are_null() {
        let tmp = std::env::temp_dir().join("h5fan-status-json");
        let paths = fixture(&tmp);
        let cfg = Config::default();
        let mut vendor = NoVendor;
        let model = build_live(&paths, &cfg, &mut vendor, 1700000000);
        let json = model.to_json();
        assert!(json.contains("\"version\""));
        assert!(json.contains("\"cpu\":{\"temp_c\":63"));
        assert!(json.contains("\"phy\":{\"temp_c\":null"));
        assert!(json.contains("\"fan\":{\"pwm_path\""));
        assert!(json.contains("\"rpm\":3100"));
        assert!(json.contains("\"thermal_owner\":\"unknown\""));
        assert!(json.contains("\"kernel_policy\":\"step_wise\""));
        assert!(json.contains("\"cpu_throttling_intact\":true"));
        assert!(json.contains("\"health\""));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn legacy_text_matches_shell_field_set() {
        let tmp = std::env::temp_dir().join("h5fan-status-text");
        let paths = fixture(&tmp);
        let cfg = Config::from_map(&HashMap::from([("mode".to_string(), "manual".to_string())]));
        let mut vendor = NoVendor;
        let model = build_live(&paths, &cfg, &mut vendor, 1700000000);
        let text = model.to_legacy_text();
        for key in [
            "enabled",
            "mode",
            "curve",
            "curve_data",
            "temp_source",
            "manual_pwm",
            "interval",
            "hysteresis",
            "down_delay",
            "start_pwm",
            "start_boost_ms",
            "pwm",
            "pwm_value",
            "fan",
            "fan_feedback",
            "fan_rpm",
            "cpu_temp",
            "cpu_label",
            "phy_temp",
            "phy_label",
            "wifi1_temp",
            "wifi1_label",
            "wifi2_temp",
            "wifi2_label",
            "module_temp",
            "module_label",
            "control_temp",
            "control_sensor",
            "thermal_floor",
            "thermal_state",
            "thermal_max_state",
            "thermal_owner",
            "result",
            "reason",
            "requested_pwm",
            "target_pwm",
            "applied_pwm",
            "state_age",
        ] {
            assert!(
                text.contains(&format!("{key}=")),
                "legacy status misses {key}"
            );
        }
        assert!(text.contains("mode=manual\n"));
        assert!(text.contains("cpu_temp=63\n"));
        assert!(text.contains("phy_temp=\n"));
        assert!(text.contains("fan_feedback=1\n"));
        assert!(text.contains("fan_rpm=3100\n"));
        assert!(!text.contains("fail_pwm="));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn fresh_snapshot_is_served_without_recomputation() {
        let tmp = std::env::temp_dir().join("h5fan-status-snap");
        let paths = fixture(&tmp);
        let mut cfg = Config::default();
        cfg.interval = 10;
        std::fs::write(
            paths.snapshot_file(),
            "{\"generated\":1700000100,\"source\":\"snapshot\"}",
        )
        .unwrap();
        let mut vendor = NoVendor;
        // 3 seconds old, interval 10 -> fresh.
        let served = json_status(&paths, &cfg, &mut vendor, 1700000103);
        assert!(served.contains("\"source\":\"snapshot\""));
        // 30 seconds old -> stale, falls back to live.
        let served = json_status(&paths, &cfg, &mut vendor, 1700000130);
        assert!(served.contains("\"source\":\"live\""));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn map_source_config_feeds_status() {
        let cfg = Config::load(&mut MapSource(HashMap::from([(
            "curve".to_string(),
            "silent".to_string(),
        )])));
        assert_eq!(cfg.profile, crate::config::Profile::Silent);
    }

    #[test]
    fn health_rules() {
        let tmp = std::env::temp_dir().join("h5fan-status-health");
        let paths = fixture(&tmp);
        let cfg = Config::default();
        let mut vendor = NoVendor;
        let mut model = build_live(&paths, &cfg, &mut vendor, 1700000000);
        // No state file -> unknown.
        assert_eq!(model.health(), "unknown");
        ControllerState {
            updated: Some(1700000000),
            result: "ok".into(),
            ..Default::default()
        }
        .write(&paths.state_file())
        .unwrap();
        model = build_live(&paths, &cfg, &mut vendor, 1700000001);
        assert_eq!(model.health(), "ok");
        model = build_live(&paths, &cfg, &mut vendor, 1700000100);
        assert_eq!(model.health(), "warn");
        ControllerState {
            updated: Some(1700000100),
            result: "failsafe".into(),
            ..Default::default()
        }
        .write(&paths.state_file())
        .unwrap();
        model = build_live(&paths, &cfg, &mut vendor, 1700000101);
        assert_eq!(model.health(), "fail");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
