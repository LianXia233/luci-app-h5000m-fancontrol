//! One fan control decision (the former shell `apply_once`).
//!
//! Decision order, hysteresis handling, down-delay semantics, start boost and
//! the kernel safety floor all reproduce the audited shell behaviour. The only
//! intentional changes: hard failures additionally persist an `error` state so
//! the LuCI diagnostics page can explain them, and the start-boost sleep is
//! chunked so signals are honoured promptly.

use crate::config::{Config, Mode};
use crate::curve;
use crate::fan;
use crate::paths::Paths;
use crate::safety::{thermal_floor, ThermalOwner};
use crate::sensor::{SensorReader, VendorRadio};
use crate::state::ControllerState;
use crate::thermal;

pub struct Controller<'a> {
    pub paths: &'a Paths,
}

pub struct Decision {
    pub requested_pwm: u32,
    pub target_pwm: u32,
    pub reason: String,
    pub result: &'static str,
}

impl<'a> Controller<'a> {
    pub fn new(paths: &'a Paths) -> Self {
        Controller { paths }
    }

    /// Runs a single control cycle. `sleep` blocks for the start-boost
    /// duration (in whole seconds) and is injected so tests run instantly.
    pub fn apply_once(
        &self,
        cfg: &Config,
        vendor: &mut dyn VendorRadio,
        now: u64,
        sleep: &dyn Fn(u64),
    ) -> Result<(), String> {
        let Some(pwm_path) = fan::find_pwm(&self.paths.pwm_root) else {
            let message = "PWM control node not found; unable to apply policy".to_string();
            log(&message);
            self.write_error_state(now, "pwm-not-found", &message);
            return Err(message);
        };

        let sensors = SensorReader::new(self.paths).read(cfg.temp_source, vendor, now);
        let floor = thermal_floor(self.paths, &pwm_path, sensors.cpu.temp);

        let mut decision = Decision {
            requested_pwm: 0,
            target_pwm: 0,
            reason: match cfg.mode {
                Mode::Auto => "auto",
                Mode::Manual => "manual",
                Mode::Kernel => "kernel",
            }
            .to_string(),
            result: "ok",
        };
        let mut down_since: u64 = 0;
        let mut owner = floor.owner.clone();

        if !cfg.enabled || cfg.mode == Mode::Kernel {
            thermal::release_thermal_control(self.paths);
            let current = fan::read_pwm(&pwm_path).unwrap_or(0);
            decision.requested_pwm = 0;
            decision.target_pwm = current;
            let state = ControllerState {
                updated: Some(now),
                result: "ok".into(),
                reason: "kernel".into(),
                requested_pwm: Some(0),
                target_pwm: Some(current),
                applied_pwm: Some(current),
                thermal_floor: Some(floor.floor),
                thermal_state: Some(floor.state),
                thermal_max_state: Some(floor.max_state),
                thermal_owner: ThermalOwner::Kernel.as_str().into(),
                control_temp: sensors.control.as_ref().and_then(|c| c.temp),
                control_sensor: sensors
                    .control
                    .as_ref()
                    .map(|c| c.label.clone())
                    .unwrap_or_default(),
                down_since: Some(0),
                last_error: String::new(),
            };
            let _ = state.write(&self.paths.state_file());
            return Ok(());
        }

        let control_state = thermal::take_thermal_control(self.paths);
        if let Err(message) = control_state {
            let message = format!(
                "unable to inspect kernel thermal policy ({message}); cooling policy not changed"
            );
            log(&message);
            thermal::release_thermal_control(self.paths);
            self.write_error_state(now, "thermal-inspect-failed", &message);
            return Err(message);
        }
        if owner == ThermalOwner::Unknown {
            owner = ThermalOwner::UserspacePwm;
        }

        let curve_points = curve::parse_curve(cfg.curve_data());
        match cfg.mode {
            Mode::Manual => {
                decision.requested_pwm = cfg.manual_pwm;
                decision.reason = "manual".into();
            }
            _ => match sensors.control.as_ref().and_then(|c| c.temp) {
                None => {
                    decision.requested_pwm = cfg.fail_pwm;
                    decision.reason = "sensor-failsafe".into();
                    decision.result = "failsafe";
                }
                Some(control_temp) => match curve_points
                    .as_deref()
                    .and_then(|points| curve::curve_pwm(control_temp, points))
                {
                    Some(pwm) => {
                        decision.requested_pwm = pwm;
                        decision.reason = "auto".into();
                    }
                    None => {
                        decision.requested_pwm = cfg.fail_pwm;
                        decision.reason = "curve-failsafe".into();
                        decision.result = "failsafe";
                    }
                },
            },
        }

        decision.requested_pwm = decision.requested_pwm.clamp(0, 255);
        decision.target_pwm = decision.requested_pwm;
        if floor.floor > decision.target_pwm {
            decision.target_pwm = floor.floor;
            decision.reason.push_str("+kernel-floor");
        }

        let current = fan::read_pwm(&pwm_path).unwrap_or(0);
        let previous = ControllerState::read(&self.paths.state_file());
        let last_pwm = previous.applied_pwm.unwrap_or(current);

        if cfg.mode == Mode::Auto && decision.result == "ok" && decision.target_pwm < last_pwm {
            // Delay speed reductions: re-evaluate the curve at
            // temp+hysteresis and hold the previous output until the delay
            // elapses.
            let control_temp = sensors
                .control
                .as_ref()
                .and_then(|c| c.temp)
                .unwrap_or_default();
            let mut candidate = curve_points
                .as_deref()
                .and_then(|points| curve::curve_pwm(control_temp + cfg.hysteresis as i32, points))
                .unwrap_or(decision.target_pwm);
            if floor.floor > candidate {
                candidate = floor.floor;
            }
            decision.target_pwm = candidate;
            down_since = previous.down_since.filter(|v| *v > 0).unwrap_or(now);
            if now.saturating_sub(down_since) < cfg.down_delay as u64 {
                decision.target_pwm = last_pwm;
                decision.reason = "auto-down-delay".into();
            } else {
                down_since = 0;
            }
        }

        decision.target_pwm = decision.target_pwm.clamp(0, 255);

        if current == 0 && decision.target_pwm > 0 && cfg.start_boost_ms > 0 {
            let boost = cfg.start_pwm.max(decision.target_pwm);
            if fan::write_pwm(&pwm_path, boost).is_err() {
                let message = format!("failed to write start boost pwm={boost}");
                log(&message);
                self.write_error_state(now, "pwm-write-failed", &message);
                return Err(message);
            }
            // BusyBox sleep accepts whole seconds only.
            sleep(cfg.start_boost_ms.div_ceil(1000) as u64);
        }

        if current != decision.target_pwm && fan::write_pwm(&pwm_path, decision.target_pwm).is_err()
        {
            let message = format!("failed to set PWM target={}", decision.target_pwm);
            log(&message);
            self.write_error_state(now, "pwm-write-failed", &message);
            return Err(message);
        }

        let applied = fan::read_pwm(&pwm_path).unwrap_or(decision.target_pwm);
        if decision.result != previous.result {
            if decision.result == "failsafe" {
                log(&format!(
                    "failsafe enabled reason={} pwm={}",
                    decision.reason, decision.target_pwm
                ));
            } else if previous.result == "failsafe" {
                log("sensor and curve validation recovered; normal control resumed");
            }
        }

        let state = ControllerState {
            updated: Some(now),
            result: decision.result.into(),
            reason: decision.reason.clone(),
            requested_pwm: Some(decision.requested_pwm),
            target_pwm: Some(decision.target_pwm),
            applied_pwm: Some(applied),
            thermal_floor: Some(floor.floor),
            thermal_state: Some(floor.state),
            thermal_max_state: Some(floor.max_state),
            thermal_owner: owner.as_str().into(),
            control_temp: sensors.control.as_ref().and_then(|c| c.temp),
            control_sensor: sensors
                .control
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_default(),
            down_since: Some(down_since),
            last_error: String::new(),
        };
        state
            .write(&self.paths.state_file())
            .map_err(|e| format!("failed to write state file: {e}"))?;
        Ok(())
    }

    fn write_error_state(&self, now: u64, reason: &str, message: &str) {
        let state = ControllerState {
            updated: Some(now),
            result: "error".into(),
            reason: reason.into(),
            last_error: message.into(),
            ..ControllerState::default()
        };
        let _ = state.write(&self.paths.state_file());
    }
}

/// Fire-and-forget syslog line, matching `logger -t h5000m-fancontrol`.
pub fn log(message: &str) {
    let _ = std::process::Command::new("logger")
        .arg("-t")
        .arg(crate::LOG_TAG)
        .arg(message)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // test fixtures patch a few paths
mod tests {
    use super::*;
    use crate::paths::Paths;
    use crate::sensor::NoVendor;
    use crate::state::ControllerState;
    use crate::uci::MapSource;
    use std::collections::HashMap;

    fn write(path: &std::path::Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn write_be32(path: &std::path::Path, words: &[u32]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
        std::fs::write(path, bytes).unwrap();
    }

    /// Shared-ownership tree: pwm-fan with cooling-levels, kernel cooling map
    /// that demands state 2 above 83°C, and a working CPU sensor.
    fn fixture(tmp: &std::path::Path) -> Paths {
        let _ = std::fs::remove_dir_all(tmp);
        let mut paths = Paths::default();
        paths.pwm_root = tmp.join("sys");
        paths.sensor_root = tmp.join("sys");
        paths.dt_root = tmp.join("dt");
        paths.state_dir = tmp.join("run");
        paths.lock_dir = tmp.join("lock/h5000m-fancontrol.lock");
        paths.radio_root = tmp.join("net");
        paths.module_cache = tmp.join("none/temperature");
        paths.legacy_cache_dir = tmp.join("none-tmp");
        std::fs::create_dir_all(&paths.state_dir).unwrap();

        let hwmon = tmp.join("sys/class/hwmon/hwmon0");
        write(&hwmon.join("name"), "pwmfan\n");
        write(&hwmon.join("pwm1"), "0\n");
        write(
            &tmp.join("sys/class/thermal/thermal_zone0/type"),
            "soc_thermal\n",
        );
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "55000\n");
        write(
            &tmp.join("sys/class/thermal/thermal_zone0/policy"),
            "step_wise\n",
        );
        write(
            &tmp.join("sys/class/thermal/thermal_zone0/available_policies"),
            "step_wise user_space\n",
        );
        let cooler = tmp.join("sys/class/thermal/cooling_device0");
        write(&cooler.join("type"), "pwm-fan\n");
        write(&cooler.join("max_state"), "3\n");

        let fan_node = tmp.join("dt/pwm-fan");
        write_be32(&fan_node.join("phandle"), &[0x2a]);
        write_be32(&fan_node.join("cooling-levels"), &[0, 96, 160, 255]);
        let zone = tmp.join("dt/thermal-zones/cpu-thermal");
        write_be32(&zone.join("trips/trip-active/phandle"), &[0x11]);
        write_be32(&zone.join("trips/trip-active/temperature"), &[83000]);
        write_be32(&zone.join("trips/trip-active/hysteresis"), &[0]);
        write_be32(&zone.join("cooling-maps/map0/cooling-device"), &[0x2a, 2]);
        write_be32(&zone.join("cooling-maps/map0/trip"), &[0x11]);
        paths
    }

    fn pwm_file(paths: &Paths) -> std::path::PathBuf {
        paths.pwm_root.join("class/hwmon/hwmon0/pwm1")
    }

    fn read_pwm(paths: &Paths) -> String {
        std::fs::read_to_string(pwm_file(paths))
            .unwrap()
            .trim()
            .to_string()
    }

    fn controller(paths: &Paths) -> Controller<'_> {
        Controller::new(paths)
    }

    const NO_SLEEP: fn(u64) = |_| {};

    fn cfg_with(entries: &[(&str, &str)]) -> Config {
        Config::load(&mut MapSource(
            entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        ))
    }

    #[test]
    fn auto_mode_applies_curve_output() {
        let tmp = std::env::temp_dir().join("h5fan-apply-auto");
        let paths = fixture(&tmp);
        let cfg = cfg_with(&[]);
        let mut vendor = NoVendor;
        // 55°C on balanced curve is the 55:60 point: 60% -> 153.
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "153");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.result, "ok");
        assert_eq!(state.reason, "auto");
        assert_eq!(state.requested_pwm, Some(153));
        assert_eq!(state.target_pwm, Some(153));
        assert_eq!(state.applied_pwm, Some(153));
        assert_eq!(state.control_temp, Some(55));
        assert_eq!(state.thermal_owner, "shared");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn manual_mode_uses_configured_pwm() {
        let tmp = std::env::temp_dir().join("h5fan-apply-manual");
        let paths = fixture(&tmp);
        let cfg = cfg_with(&[("mode", "manual"), ("manual_pwm", "200")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "200");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.reason, "manual");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kernel_mode_leaves_output_untouched() {
        let tmp = std::env::temp_dir().join("h5fan-apply-kernel");
        let paths = fixture(&tmp);
        write(&pwm_file(&paths), "180\n");
        let cfg = cfg_with(&[("mode", "kernel")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "180");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.reason, "kernel");
        assert_eq!(state.thermal_owner, "kernel");
        assert_eq!(state.requested_pwm, Some(0));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn disabled_controller_switches_to_kernel_policies() {
        let tmp = std::env::temp_dir().join("h5fan-apply-disabled");
        let paths = fixture(&tmp);
        write(&pwm_file(&paths), "120\n");
        let cfg = cfg_with(&[("enabled", "0")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "120");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.thermal_owner, "kernel");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_sensor_triggers_failsafe_full_output() {
        let tmp = std::env::temp_dir().join("h5fan-apply-nosensor");
        let paths = fixture(&tmp);
        std::fs::remove_file(tmp.join("sys/class/thermal/thermal_zone0/temp")).unwrap();
        let cfg = cfg_with(&[("fail_pwm", "200")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        // The shared kernel cooling map cannot be evaluated without a CPU
        // reading, so the safety floor escalates the 200 PWM to full speed.
        assert_eq!(read_pwm(&paths), "255");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.result, "failsafe");
        assert_eq!(state.reason, "sensor-failsafe+kernel-floor");
        assert_eq!(state.requested_pwm, Some(200));
        assert_eq!(state.control_temp, None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn invalid_curve_triggers_curve_failsafe() {
        let tmp = std::env::temp_dir().join("h5fan-apply-badcurve");
        let paths = fixture(&tmp);
        let cfg = cfg_with(&[("curve", "custom"), ("curve_custom", "40:50,30:60")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "255");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.result, "failsafe");
        assert_eq!(state.reason, "curve-failsafe");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kernel_floor_raises_low_curve_output() {
        let tmp = std::env::temp_dir().join("h5fan-apply-floor");
        let paths = fixture(&tmp);
        // 90°C: the kernel map at 83°C demands state 2 (PWM 160); the quiet
        // test curve only reaches 28% (71), so the floor wins.
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "90000\n");
        let cfg = cfg_with(&[("curve", "silent"), ("curve_silent", "20:0,50:20,95:30")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "160");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.reason, "auto+kernel-floor");
        assert_eq!(state.target_pwm, Some(160));
        assert_eq!(state.requested_pwm, Some(71));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn down_delay_holds_previous_output() {
        let tmp = std::env::temp_dir().join("h5fan-apply-down-delay");
        let paths = fixture(&tmp);
        // First cycle at 60°C: 60 + 5*15/15 = 65% -> 165.
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "60000\n");
        let cfg = cfg_with(&[("down_delay", "30"), ("hysteresis", "2")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "165");
        // Temperature drops to 45°C (hysteresis candidate 47°C -> 54% -> 137),
        // but the 30 s delay holds the previous output.
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "45000\n");
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1010, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "165");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.reason, "auto-down-delay");
        // The countdown starts when the first reduction is attempted.
        assert_eq!(state.down_since, Some(1010));
        // After the delay elapses the hysteresis-adjusted candidate applies:
        // 47°C -> 54% -> 137 (a further cycle would settle at the 45°C value).
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1041, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "137");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.reason, "auto");
        assert_eq!(state.down_since, Some(0));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn speed_increases_apply_immediately() {
        let tmp = std::env::temp_dir().join("h5fan-apply-up");
        let paths = fixture(&tmp);
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "45000\n");
        let cfg = cfg_with(&[]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "135");
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "70000\n");
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1001, &NO_SLEEP)
            .unwrap();
        // 70°C is a curve point: 75% -> 191 immediately, no delay.
        assert_eq!(read_pwm(&paths), "191");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn start_boost_kicks_stopped_fan_then_settles() {
        let tmp = std::env::temp_dir().join("h5fan-apply-boost");
        let paths = fixture(&tmp);
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "55000\n");
        let cfg = cfg_with(&[("start_pwm", "220"), ("start_boost_ms", "700")]);
        let mut vendor = NoVendor;
        let boosts = std::cell::RefCell::new(Vec::new());
        let sleep = |secs: u64| {
            boosts.borrow_mut().push(secs);
        };
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &sleep)
            .unwrap();
        // 700 ms rounds up to one whole second of boost.
        assert_eq!(boosts.borrow().as_slice(), &[1u64]);
        // The boost wrote max(220, 153) = 220 first, then the target 153.
        assert_eq!(read_pwm(&paths), "153");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_pwm_node_reports_error_state() {
        let tmp = std::env::temp_dir().join("h5fan-apply-nopwm");
        let paths = fixture(&tmp);
        std::fs::remove_file(pwm_file(&paths)).unwrap();
        let cfg = cfg_with(&[]);
        let mut vendor = NoVendor;
        let err = controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap_err();
        assert!(err.contains("PWM control node not found"));
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.result, "error");
        assert_eq!(state.reason, "pwm-not-found");
        assert!(state.last_error.contains("PWM control node not found"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn exclusive_ownership_uses_emergency_floors() {
        let tmp = std::env::temp_dir().join("h5fan-apply-exclusive");
        let paths = fixture(&tmp);
        // Detach the fan from the kernel cooling map (patch applied).
        let map = tmp.join("dt/thermal-zones/cpu-thermal/cooling-maps/map0/cooling-device");
        write_be32(&map, &[0x99, 2]);
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "90000\n");
        let cfg = cfg_with(&[("curve", "silent"), ("curve_silent", "20:0,95:100")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        // 90°C on the quiet curve: 0 + 70*100/75 = 93% -> 237; the userspace
        // emergency floor at 83°C only demands 160, so the curve stays in charge.
        assert_eq!(read_pwm(&paths), "237");
        let state = ControllerState::read(&paths.state_file());
        // Exclusive ownership was derived from the cooling maps, so the owner
        // stays "userspace" (the "userspace-pwm" tag only appears when no
        // pwm-fan cooling device exists at all).
        assert_eq!(state.thermal_owner, "userspace");
        // Missing CPU sensor under exclusive ownership -> full speed.
        std::fs::remove_file(tmp.join("sys/class/thermal/thermal_zone0/temp")).unwrap();
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1001, &NO_SLEEP)
            .unwrap();
        assert_eq!(read_pwm(&paths), "255");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn custom_profile_selection_respects_curve() {
        let tmp = std::env::temp_dir().join("h5fan-apply-custom");
        let paths = fixture(&tmp);
        let cfg = cfg_with(&[("curve", "custom"), ("curve_custom", "20:0,55:100")]);
        let mut vendor = NoVendor;
        controller(&paths)
            .apply_once(&cfg, &mut vendor, 1000, &NO_SLEEP)
            .unwrap();
        // 55°C at 100% -> 255.
        assert_eq!(read_pwm(&paths), "255");
        let state = ControllerState::read(&paths.state_file());
        assert_eq!(state.result, "ok");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
