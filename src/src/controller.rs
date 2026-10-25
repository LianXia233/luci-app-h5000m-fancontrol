//! Control policy: decide what the fan should do, then apply it.
//!
//! [`decide`] is pure: the same settings, sensors, floor and previous output
//! always produce the same target. That is what makes the hysteresis,
//! spin-down delay and failsafe paths testable without touching hardware.
//! [`actuate`] is the only place that writes a PWM register, and [`run_cycle`]
//! wires the pieces together for both the daemon and `h5000m-fancontrol apply`.

use crate::config::{Mode, Settings};
use crate::curve::{self, CurvePoint};
use crate::error::{Error, Result};
use crate::fan::{self, FanDevice};
use crate::paths::Paths;
use crate::safety::{self, Ownership};
use crate::sensor::{self, RadioProbe, SensorId, SensorReading, Sensors};
use crate::state::{ControlResult, Snapshot};
use std::time::Duration;

/// Reason codes are wire compatible with the 2.x controller: the LuCI view and
/// any external script keep matching on exactly these strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Auto,
    Manual,
    Kernel,
    SensorFailsafe,
    CurveFailsafe,
    AutoDownDelay,
    PwmMissing,
    WriteFailed,
    CycleFailed,
}

impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Reason::Auto => "auto",
            Reason::Manual => "manual",
            Reason::Kernel => "kernel",
            Reason::SensorFailsafe => "sensor-failsafe",
            Reason::CurveFailsafe => "curve-failsafe",
            Reason::AutoDownDelay => "auto-down-delay",
            Reason::PwmMissing => "pwm-missing",
            Reason::WriteFailed => "pwm-write-failed",
            Reason::CycleFailed => "cycle-failed",
        }
    }

    pub fn with_floor(self, floor_applied: bool) -> String {
        if floor_applied {
            format!("{}+kernel-floor", self.code())
        } else {
            self.code().to_string()
        }
    }
}

/// A snapshot older than this is not trusted for the spin-down memory, because
/// the fan may have been driven by the kernel in the meantime.
pub const TRUST_STATE_SECONDS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleInput<'a> {
    pub settings: &'a Settings,
    pub sensors: &'a Sensors,
    pub floor: u8,
    pub current_pwm: u8,
    /// `applied_pwm` from a still trusted snapshot, if any.
    pub last_applied: Option<u8>,
    pub down_since: u64,
    pub now: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub requested: u8,
    pub target: u8,
    pub reason: Reason,
    pub result: ControlResult,
    pub kernel_floor_applied: bool,
    pub down_since: u64,
    /// The target was held at the previous output to avoid a premature slowdown.
    pub holding_previous: bool,
    pub control_temp: Option<i32>,
    pub control_sensor: Option<String>,
    pub control_source: Option<String>,
    pub notes: Vec<String>,
}

impl Decision {
    pub fn reason_text(&self) -> String {
        self.reason.with_floor(self.kernel_floor_applied)
    }
}

fn apply_floor(requested: u8, floor: u8, notes: &mut Vec<String>) -> (u8, bool) {
    if floor > requested {
        notes.push(format!("kernel floor {floor}/255 raised the request"));
        (floor, true)
    } else {
        (requested, false)
    }
}

fn failsafe(settings: &Settings, floor: u8, reason: Reason, note: String) -> Decision {
    let mut notes = vec![note];
    let (target, raised) = apply_floor(settings.fail_pwm, floor, &mut notes);
    Decision {
        requested: settings.fail_pwm,
        target,
        reason,
        result: ControlResult::Failsafe,
        kernel_floor_applied: raised,
        down_since: 0,
        holding_previous: false,
        control_temp: None,
        control_sensor: None,
        control_source: None,
        notes,
    }
}

/// Pure policy evaluation for one cycle.
pub fn decide(input: &CycleInput) -> Decision {
    let settings = input.settings;
    let mut notes = Vec::new();
    let control = input
        .sensors
        .control(settings)
        .map(|(reading, value)| (value, reading.label.clone(), reading.source.clone()));

    if settings.is_kernel_only() {
        let (control_temp, control_sensor, control_source) = control.clone().unwrap_or_default();
        return Decision {
            requested: 0,
            target: input.current_pwm,
            reason: Reason::Kernel,
            result: ControlResult::Ok,
            kernel_floor_applied: false,
            down_since: 0,
            holding_previous: false,
            control_temp: if control_sensor.is_none() {
                None
            } else {
                Some(control_temp)
            },
            control_sensor,
            control_source,
            notes: if settings.enabled {
                vec!["kernel protection only mode".to_string()]
            } else {
                vec!["controller disabled; the kernel drives the fan".to_string()]
            },
        };
    }

    let Some((control_temp, control_label, control_source)) = control else {
        return failsafe(
            settings,
            input.floor,
            Reason::SensorFailsafe,
            "no valid temperature sensor is readable".to_string(),
        );
    };

    if settings.mode == Mode::Manual {
        let (target, raised) = apply_floor(settings.manual_pwm, input.floor, &mut notes);
        return Decision {
            requested: settings.manual_pwm,
            target,
            reason: Reason::Manual,
            result: ControlResult::Ok,
            kernel_floor_applied: raised,
            down_since: 0,
            holding_previous: false,
            control_temp: Some(control_temp),
            control_sensor: control_label,
            control_source,
            notes,
        };
    }

    let points: Vec<CurvePoint> = match settings.active_curve() {
        Ok(points) => points,
        Err(err) => {
            return failsafe(
                settings,
                input.floor,
                Reason::CurveFailsafe,
                format!("curve {:?} is not usable: {err}", settings.curve_spec()),
            );
        }
    };

    let Some(base) = curve::pwm_for(&points, control_temp) else {
        return failsafe(
            settings,
            input.floor,
            Reason::CurveFailsafe,
            "curve evaluation produced no output".to_string(),
        );
    };

    let mut reason = Reason::Auto;
    let mut target = base;
    let mut kernel_floor_applied = false;
    if input.floor > target {
        target = input.floor;
        kernel_floor_applied = true;
        notes.push(format!(
            "kernel floor {}/255 raised the request",
            input.floor
        ));
    }
    let mut down_since = 0u64;
    let mut holding_previous = false;

    let last = input.last_applied.unwrap_or(input.current_pwm);
    // Only automatic control that is not in a failsafe may slow down, and only
    // after the configured delay elapsed. Failsafe paths returned above.
    if settings.mode == Mode::Auto && target < last {
        let hysteresis_temp = control_temp.saturating_add(settings.hysteresis);
        let mut candidate = curve::pwm_for(&points, hysteresis_temp).unwrap_or(target);
        if input.floor > candidate {
            candidate = input.floor;
            kernel_floor_applied = true;
        }
        target = candidate;
        let start = if input.down_since > 0 {
            input.down_since
        } else {
            input.now
        };
        let elapsed = input.now.saturating_sub(start);
        if elapsed < settings.down_delay {
            target = last;
            reason = Reason::AutoDownDelay;
            holding_previous = true;
            down_since = start;
            notes.push(format!(
                "holding the previous output for {} s",
                settings.down_delay - elapsed
            ));
            if input.floor > target {
                kernel_floor_applied = true;
            }
        }
    }

    Decision {
        requested: base,
        target,
        reason,
        result: ControlResult::Ok,
        kernel_floor_applied,
        down_since,
        holding_previous,
        control_temp: Some(control_temp),
        control_sensor: control_label,
        control_source,
        notes,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actuation {
    /// Values written, in order.
    pub writes: Vec<u8>,
    pub boost_applied: bool,
    pub applied_pwm: u8,
    /// Whether the register reads back exactly what was written. A driver that
    /// clamps the value is reported honestly instead of silently.
    pub verified: bool,
}

/// Write the decision to hardware.
///
/// `pause` is injected so the startup boost can be tested without sleeping and
/// so the daemon can cut a boost short when it is asked to stop.
pub fn actuate(
    fan: &FanDevice,
    settings: &Settings,
    decision: &Decision,
    current_pwm: Option<u8>,
    pause: &mut dyn FnMut(Duration),
) -> Result<Actuation> {
    let mut writes = Vec::new();
    let mut boost_applied = false;
    let target = decision.target;

    // A register that cannot be read is treated as stopped, so the boost still
    // runs: briefly over-driving a spinning fan is harmless, a fan that fails to
    // start is not.
    if current_pwm.unwrap_or(0) == 0 && target > 0 && settings.start_boost_ms > 0 {
        let boost = settings.start_pwm.max(target);
        fan.write_pwm(boost)?;
        writes.push(boost);
        boost_applied = true;
        pause(Duration::from_millis(settings.start_boost_ms));
    }

    if current_pwm != Some(target) {
        let readback = fan.write_pwm(target)?;
        writes.push(target);
        return Ok(Actuation {
            writes,
            boost_applied,
            applied_pwm: readback,
            verified: readback == target,
        });
    }

    Ok(Actuation {
        writes,
        boost_applied,
        applied_pwm: fan.read_pwm().unwrap_or(target),
        verified: true,
    })
}

/// One complete control cycle, including publishing the state snapshot.
pub fn run_cycle(
    paths: &Paths,
    probe: &dyn RadioProbe,
    previous: Option<&Snapshot>,
    now: u64,
    pause: &mut dyn FnMut(Duration),
) -> Snapshot {
    let settings = match Settings::load(paths) {
        Ok(settings) => settings,
        Err(err) => {
            return failure_snapshot(
                Snapshot::default(),
                Reason::CycleFailed,
                &err,
                previous,
                now,
            );
        }
    };

    let Some(device) = fan::discover(paths) else {
        let snapshot = failure_snapshot(
            Snapshot::default(),
            Reason::PwmMissing,
            &Error::PwmMissing,
            previous,
            now,
        );
        return with_settings(snapshot, &settings, now, previous);
    };

    let sensors = sensor::sample_all(paths, probe, now);
    let floor = safety::evaluate_floor(paths, &device, sensors.value(SensorId::Cpu));
    let current_pwm = device.read_pwm();
    let trusted_previous = match previous {
        Some(snapshot)
            if snapshot.updated > 0
                && now.saturating_sub(snapshot.updated) <= TRUST_STATE_SECONDS =>
        {
            Some(snapshot)
        }
        _ => None,
    };

    let input = CycleInput {
        settings: &settings,
        sensors: &sensors,
        floor: floor.floor_pwm,
        current_pwm: current_pwm.unwrap_or(0),
        last_applied: trusted_previous.map(|snapshot| snapshot.applied_pwm),
        down_since: trusted_previous
            .map(|snapshot| snapshot.down_since)
            .unwrap_or(0),
        now,
    };
    let decision = decide(&input);

    let mut snapshot = Snapshot::published_now(now);
    snapshot.thermal_floor = floor.floor_pwm;
    snapshot.thermal_state = floor.thermal_state;
    snapshot.thermal_max_state = floor.max_state;
    snapshot.thermal_owner = if settings.is_kernel_only() {
        Ownership::Kernel.as_str().to_string()
    } else {
        floor.ownership.as_apply_str().to_string()
    };
    snapshot.thermal_basis = floor.basis.clone();
    snapshot.pwm_path = Some(device.pwm_path.display().to_string());
    snapshot.fan_path = device.rpm_path.as_ref().map(|p| p.display().to_string());
    snapshot.rpm = device.read_rpm();
    snapshot.sensors = sensors.readings.clone();
    snapshot.control_temp = decision.control_temp;
    snapshot.control_sensor = decision.control_sensor.clone();
    snapshot.control_source = decision.control_source.clone();

    if settings.is_kernel_only() {
        // Nothing is written: the kernel owns the output. Report what is there.
        snapshot.applied_pwm = current_pwm.unwrap_or(0);
        snapshot.target_pwm = snapshot.applied_pwm;
        snapshot.requested_pwm = 0;
        snapshot.reason = decision.reason_text();
        snapshot.result = decision.result;
        snapshot.down_since = 0;
        snapshot.last_error = previous
            .filter(|p| p.result == ControlResult::Error)
            .and_then(|p| p.last_error.clone());
        return with_settings(snapshot, &settings, now, previous);
    }

    match actuate(&device, &settings, &decision, current_pwm, pause) {
        Ok(actuation) => {
            snapshot.requested_pwm = decision.requested;
            snapshot.target_pwm = decision.target;
            snapshot.applied_pwm = actuation.applied_pwm;
            snapshot.reason = decision.reason_text();
            snapshot.result = decision.result;
            snapshot.kernel_floor_applied = decision.kernel_floor_applied;
            snapshot.down_since = decision.down_since;
            snapshot.boost_applied = actuation.boost_applied;
            if !actuation.verified {
                snapshot.last_error = Some(format!(
                    "{} reads back {} instead of {}",
                    device.pwm_path.display(),
                    actuation.applied_pwm,
                    decision.target
                ));
            } else {
                snapshot.last_error = None;
            }
            with_settings(snapshot, &settings, now, previous)
        }
        Err(err) => {
            let snapshot = failure_snapshot(snapshot, Reason::WriteFailed, &err, previous, now);
            with_settings(snapshot, &settings, now, previous)
        }
    }
}

fn with_settings(
    mut snapshot: Snapshot,
    settings: &Settings,
    now: u64,
    previous: Option<&Snapshot>,
) -> Snapshot {
    snapshot.policy_signature = settings.policy_signature();
    snapshot.cycle_count = previous.map(|p| p.cycle_count).unwrap_or(0) + 1;
    snapshot.consecutive_failures = match snapshot.result {
        ControlResult::Error => previous.map(|p| p.consecutive_failures).unwrap_or(0) + 1,
        _ => 0,
    };
    snapshot.last_good_cycle = match snapshot.result {
        ControlResult::Error => previous.map(|p| p.last_good_cycle).unwrap_or(0),
        _ => now,
    };
    snapshot
}

/// Snapshot for a cycle that could not reach the hardware at all.
pub fn failure_snapshot(
    mut snapshot: Snapshot,
    reason: Reason,
    error: &Error,
    previous: Option<&Snapshot>,
    now: u64,
) -> Snapshot {
    snapshot.updated = now;
    snapshot.pid = crate::platform::process_id();
    snapshot.result = ControlResult::Error;
    snapshot.reason = reason.code().to_string();
    snapshot.last_error = Some(error.to_string());
    snapshot.cycle_count = previous.map(|p| p.cycle_count).unwrap_or(0) + 1;
    snapshot.consecutive_failures = previous.map(|p| p.consecutive_failures).unwrap_or(0) + 1;
    snapshot.last_good_cycle = previous.map(|p| p.last_good_cycle).unwrap_or(0);
    snapshot.thermal_owner = Ownership::Unknown.as_str().to_string();
    snapshot
}

/// Publish the snapshot after a cycle, keeping the on-disk state consistent even
/// when the write fails (the caller still gets the in-memory result).
pub fn publish(paths: &Paths, snapshot: &Snapshot) -> Result<()> {
    snapshot.write(&paths.state_file())
}

/// Helper used by the status report: rebuild a reading list when no snapshot
/// exists yet (fresh install, or the daemon never ran). The rows stay present
/// and explain themselves, instead of leaving a client to invent a default.
pub fn placeholder_readings(sampled_at: u64) -> Vec<SensorReading> {
    SensorId::all()
        .iter()
        .map(|id| {
            SensorReading::new(*id, sampled_at).with_problem(
                crate::sensor::SensorState::Missing,
                "the controller has not published a sample yet".to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uci::UciFile;

    fn cfg(text: &str) -> Settings {
        Settings::from_uci(&UciFile::parse(&format!(
            "config settings 'settings'\n{text}"
        )))
    }

    fn sensors(values: [(SensorId, Option<i32>); 5]) -> Sensors {
        Sensors {
            readings: values
                .iter()
                .map(|(id, value)| match value {
                    Some(value) => SensorReading::new(*id, 1000).with_value(
                        *value,
                        id.title().into(),
                        "test".into(),
                        "/test".into(),
                    ),
                    None => SensorReading::new(*id, 1000),
                })
                .collect(),
        }
    }

    fn cpu_only(temp: i32) -> Sensors {
        sensors([
            (SensorId::Cpu, Some(temp)),
            (SensorId::Phy, None),
            (SensorId::Wifi1, None),
            (SensorId::Wifi2, None),
            (SensorId::Module, None),
        ])
    }

    fn no_sensors() -> Sensors {
        sensors([
            (SensorId::Cpu, None),
            (SensorId::Phy, None),
            (SensorId::Wifi1, None),
            (SensorId::Wifi2, None),
            (SensorId::Module, None),
        ])
    }

    fn input<'a>(settings: &'a Settings, sensors: &'a Sensors) -> CycleInput<'a> {
        CycleInput {
            settings,
            sensors,
            floor: 0,
            current_pwm: 0,
            last_applied: None,
            down_since: 0,
            now: 1000,
        }
    }

    fn decide_on(settings: &Settings, sensors: &Sensors) -> Decision {
        decide(&input(settings, sensors))
    }

    #[test]
    fn disabled_mode_never_moves_the_fan() {
        let settings = cfg("\toption enabled '0'\n");
        let sensors = cpu_only(90);
        let mut input = input(&settings, &sensors);
        input.current_pwm = 77;
        let decision = decide(&input);
        assert_eq!(decision.reason, Reason::Kernel);
        assert_eq!(decision.target, 77, "the kernel keeps its own output");
        assert_eq!(decision.requested, 0);
        assert_eq!(decision.result, ControlResult::Ok);
    }

    #[test]
    fn kernel_mode_reports_the_observed_temperature() {
        let settings = cfg("\toption mode 'kernel'\n");
        let sensors = cpu_only(90);
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.reason, Reason::Kernel);
        assert_eq!(decision.control_temp, Some(90));
        assert_eq!(decision.target, 0);
    }

    #[test]
    fn manual_mode_uses_the_configured_output() {
        let settings = cfg("\toption mode 'manual'\n\toption manual_pwm '100'\n");
        let sensors = cpu_only(20);
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.requested, 100);
        assert_eq!(decision.target, 100);
        assert_eq!(decision.reason, Reason::Manual);
        assert_eq!(decision.reason_text(), "manual");
    }

    #[test]
    fn missing_sensor_forces_the_high_output_failsafe() {
        let settings = cfg("");
        let sensors = no_sensors();
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.reason, Reason::SensorFailsafe);
        assert_eq!(decision.result, ControlResult::Failsafe);
        assert_eq!(decision.requested, 255);
        assert_eq!(decision.control_temp, None);
    }

    #[test]
    fn invalid_curve_forces_the_high_output_failsafe() {
        let settings = cfg("\toption curve 'custom'\n\toption curve_custom '50:100,40:80'\n");
        let sensors = cpu_only(60);
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.reason, Reason::CurveFailsafe);
        assert_eq!(decision.requested, settings.fail_pwm);
        assert_eq!(decision.result, ControlResult::Failsafe);
    }

    #[test]
    fn automatic_mode_follows_the_curve_and_reports_the_reason() {
        let settings = cfg("\toption curve 'balanced'\n");
        let sensors = cpu_only(55);
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.reason, Reason::Auto);
        assert_eq!(decision.requested, (60u16 * 255 / 100) as u8);
        assert_eq!(decision.result, ControlResult::Ok);
        assert_eq!(decision.control_temp, Some(55));
    }

    #[test]
    fn kernel_floor_raises_the_target_and_is_labelled() {
        let settings = cfg("\toption curve 'balanced'\n");
        let sensors = cpu_only(45);
        let mut input = input(&settings, &sensors);
        input.floor = 200;
        let decision = decide(&input);
        let curve = settings.active_curve().unwrap();
        assert_eq!(
            decision.requested,
            curve::pwm_for(&curve, 45).unwrap(),
            "the request stays what the curve asked for"
        );
        assert_eq!(decision.target, 200);
        assert!(decision.kernel_floor_applied);
        assert_eq!(decision.reason_text(), "auto+kernel-floor");
    }

    #[test]
    fn manual_requests_are_still_bounded_by_the_kernel_floor() {
        let settings = cfg("\toption mode 'manual'\n\toption manual_pwm '10'\n");
        let sensors = cpu_only(90);
        let mut input = input(&settings, &sensors);
        input.floor = 180;
        let decision = decide(&input);
        assert_eq!(decision.target, 180);
        assert!(decision.kernel_floor_applied);
        assert_eq!(decision.reason_text(), "manual+kernel-floor");
    }

    #[test]
    fn spin_down_is_delayed_and_holds_the_previous_output() {
        let settings = cfg("\toption down_delay '30'\n\toption hysteresis '2'\n");
        let sensors = cpu_only(40);
        let mut input = input(&settings, &sensors);
        input.current_pwm = 200;
        input.last_applied = Some(200);
        let decision = decide(&input);
        assert_eq!(decision.reason, Reason::AutoDownDelay);
        assert_eq!(decision.target, 200, "the fan must not slow down yet");
        assert_eq!(decision.down_since, 1000, "the delay clock starts now");
        assert!(decision.holding_previous);

        input.down_since = 970;
        let decision = decide(&input);
        assert_eq!(decision.reason, Reason::Auto);
        assert_eq!(decision.down_since, 0);
        assert!(decision.target < 200);
    }

    #[test]
    fn spin_up_is_immediate_even_while_a_delay_is_running() {
        let settings = cfg("\toption down_delay '300'\n");
        let sensors = cpu_only(88);
        let mut input = input(&settings, &sensors);
        input.current_pwm = 100;
        input.last_applied = Some(100);
        input.down_since = 900;
        let decision = decide(&input);
        assert_eq!(decision.reason, Reason::Auto);
        assert!(decision.target > 100);
        assert_eq!(decision.down_since, 0);
    }

    #[test]
    fn hysteresis_only_ever_raises_the_slow_down_target() {
        let cold = cfg("\toption hysteresis '0'\n\toption down_delay '5'\n");
        let warm = cfg("\toption hysteresis '10'\n\toption down_delay '5'\n");
        let sensors = cpu_only(60);

        let mut base = input(&cold, &sensors);
        base.current_pwm = 255;
        base.last_applied = Some(255);
        // The delay already expired, so the curve decides - with the CPU reading
        // lifted by the hysteresis.
        base.down_since = 900;
        let plain = decide(&base);

        let mut warm_input = input(&warm, &sensors);
        warm_input.current_pwm = 255;
        warm_input.last_applied = Some(255);
        warm_input.down_since = 900;
        let with_hysteresis = decide(&warm_input);

        assert_eq!(plain.reason, Reason::Auto);
        assert_eq!(with_hysteresis.reason, Reason::Auto);
        assert!(
            with_hysteresis.target > plain.target,
            "hysteresis must never cool less: {} < {}",
            with_hysteresis.target,
            plain.target
        );
    }

    #[test]
    fn failsafe_output_is_not_subject_to_the_spin_down_delay() {
        let settings = cfg("\toption down_delay '300'\n\toption fail_pwm '255'\n");
        let sensors = no_sensors();
        let mut input = input(&settings, &sensors);
        input.current_pwm = 20;
        input.last_applied = Some(20);
        let decision = decide(&input);
        assert_eq!(decision.reason, Reason::SensorFailsafe);
        assert_eq!(decision.target, 255);
        assert_eq!(decision.down_since, 0);
    }

    #[test]
    fn control_temperature_uses_the_hottest_valid_sensor() {
        let settings = cfg("");
        let sensors = sensors([
            (SensorId::Cpu, Some(52)),
            (SensorId::Phy, Some(61)),
            (SensorId::Wifi1, Some(58)),
            (SensorId::Wifi2, Some(49)),
            (SensorId::Module, Some(40)),
        ]);
        let decision = decide_on(&settings, &sensors);
        let curve = settings.active_curve().unwrap();
        assert_eq!(decision.requested, curve::pwm_for(&curve, 61).unwrap());
        assert_eq!(decision.control_temp, Some(61));
    }

    #[test]
    fn stale_and_invalid_sensors_cannot_be_control_inputs() {
        let settings = cfg("");
        let mut sensors = cpu_only(70);
        sensors.readings[0].state = crate::sensor::SensorState::Stale;
        sensors.readings[0].value_c = None;
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.reason, Reason::SensorFailsafe);
    }

    #[test]
    fn control_source_is_carried_into_the_decision() {
        let settings = cfg("");
        let sensors = cpu_only(70);
        let decision = decide_on(&settings, &sensors);
        assert_eq!(decision.control_source.as_deref(), Some("test"));
        assert_eq!(decision.control_sensor.as_deref(), Some("CPU"));
    }
}
