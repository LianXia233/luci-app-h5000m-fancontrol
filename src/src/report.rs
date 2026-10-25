//! Status model and its two serialisations.
//!
//! `json` is the contract the LuCI view consumes: typed fields, `null` for
//! anything unknown, and an explicit per-sensor status. `plain` keeps every key
//! the 2.x controller emitted so existing shell consumers keep parsing it, with
//! the new fields appended.

use crate::config::Settings;
use crate::events::{self, Event};
use crate::fan;
use crate::json::Value;
use crate::paths::Paths;
use crate::safety::{self, Ownership};
use crate::sensor::{RadioProbe, SensorId, SensorReading, Sensors};
use crate::state::{ControlResult, Snapshot};

pub const REPORT_SCHEMA: u64 = 3;
pub const EVENT_LIMIT: usize = 40;

/// How the reported temperatures were obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleMode {
    /// Reuse the snapshot the daemon publishes: no sysfs walk, no helper call.
    Cached,
    /// Sample everything now, as `h5000m-fancontrol status live` does.
    Live,
}

impl SampleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SampleMode::Cached => "cached",
            SampleMode::Live => "live",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// The loop is running and applying the policy.
    Running,
    /// A safety path is active.
    Failsafe,
    /// No fresh snapshot: the daemon is not running or is wedged.
    Stale,
    /// The last cycle failed.
    Error,
    /// Never published yet.
    Stopped,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Running => "running",
            Health::Failsafe => "failsafe",
            Health::Stale => "stale",
            Health::Error => "error",
            Health::Stopped => "stopped",
        }
    }
}

/// Percent of the 8 bit range, rounded the same way the frontend does.
pub fn percent_of_pwm(pwm: u8) -> u32 {
    (pwm as u32 * 200 + 255) / 510
}

pub fn compute_health(snapshot: Option<&Snapshot>, now: u64, interval: u64) -> Health {
    let Some(snapshot) = snapshot else {
        return Health::Stopped;
    };
    if snapshot.result == ControlResult::Error {
        return Health::Error;
    }
    if snapshot.is_stale(now, interval) {
        return Health::Stale;
    }
    match snapshot.result {
        ControlResult::Failsafe => Health::Failsafe,
        _ => Health::Running,
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub now: u64,
    pub mode: SampleMode,
    pub settings: Settings,
    pub snapshot: Option<Snapshot>,
    pub sensors: Vec<SensorReading>,
    pub control_temp: Option<i32>,
    pub control_sensor: Option<String>,
    pub control_source: Option<String>,
    pub pwm_path: Option<String>,
    pub pwm_value: Option<u32>,
    pub pwm_max: Option<u32>,
    pub rpm: Option<u32>,
    pub fan_path: Option<String>,
    pub thermal_floor: u8,
    pub thermal_state: u32,
    pub thermal_max_state: u32,
    pub thermal_owner: String,
    pub thermal_basis: String,
    pub cooling_levels: Vec<u32>,
    pub map_states: Vec<(u32, i32)>,
    pub protection: safety::KernelProtection,
    pub legacy_policy_pending: bool,
    pub events: Vec<Event>,
    pub daemon_pid: Option<u32>,
    pub requested_pwm: u8,
    pub target_pwm: u8,
    pub applied_pwm: u8,
    pub reason: String,
    pub kernel_floor_applied: bool,
    pub boost_applied: bool,
    pub down_since: u64,
    pub result: ControlResult,
    pub last_error: Option<String>,
    pub cycle_count: u64,
    pub consecutive_failures: u64,
    pub last_good_cycle: u64,
    pub updated_at: u64,
    pub state_age: Option<u64>,
    /// Whether a control cycle ever published a snapshot.
    pub published: bool,
}

/// `key=` for an unknown value: empty, never a fabricated zero.
fn show<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map(|inner| inner.to_string()).unwrap_or_default()
}

impl Report {
    pub fn health(&self) -> Health {
        compute_health(self.snapshot.as_ref(), self.now, self.settings.interval)
    }

    /// Seconds still left on an active spin-down hold.
    pub fn down_delay_remaining(&self) -> u64 {
        if self.down_since == 0 || !self.reason.starts_with("auto-down-delay") {
            return 0;
        }
        let elapsed = self.now.saturating_sub(self.down_since);
        self.settings.down_delay.saturating_sub(elapsed)
    }

    /// True when no sensor at all can be trusted.
    pub fn sensor_failure(&self) -> bool {
        self.sensors.iter().all(|reading| !reading.is_valid())
    }

    pub fn to_value(&self, paths: &Paths) -> Value {
        let sensors = Value::array(self.sensors.iter().map(|reading| {
            let age = if reading.sampled_at == 0 {
                None
            } else {
                Some(self.now.saturating_sub(reading.sampled_at) as i64)
            };
            Value::object([
                ("id", Value::str(reading.id.key())),
                ("title", Value::str(reading.id.title())),
                ("value_c", Value::opt_int(reading.value_c)),
                ("status", Value::str(reading.state.as_str())),
                ("valid", Value::Bool(reading.is_valid())),
                ("label", Value::opt_str(reading.label.clone())),
                ("source", Value::opt_str(reading.source.clone())),
                ("path", Value::opt_str(reading.path.clone())),
                ("error", Value::opt_str(reading.error.clone())),
                ("sampled_at", Value::Int(reading.sampled_at as i64)),
                ("age_s", Value::opt_int(age)),
            ])
        }));

        let issues = Value::array(self.settings.issues.iter().map(|issue| {
            Value::object([
                ("option", Value::str(issue.option.clone())),
                ("value", Value::str(issue.value.clone())),
                ("problem", Value::str(issue.problem.clone())),
                ("applied", Value::str(issue.applied.clone())),
            ])
        }));

        let trips = Value::array(self.protection.trips.iter().map(|trip| {
            Value::object([
                ("zone", Value::str(trip.zone.clone())),
                ("name", Value::str(trip.name.clone())),
                ("type", Value::str(trip.kind.clone())),
                ("temp_c", Value::opt_int(trip.temp_c)),
                ("hysteresis_c", Value::Int(trip.hysteresis_c as i64)),
            ])
        }));

        Value::object([
            ("schema", Value::Int(REPORT_SCHEMA as i64)),
            ("generated_at", Value::Int(self.now as i64)),
            ("sample_mode", Value::str(self.mode.as_str())),
            (
                "controller",
                Value::object([
                    ("version", Value::str(crate::VERSION)),
                    ("health", Value::str(self.health().as_str())),
                    ("running", Value::Bool(self.daemon_pid.is_some())),
                    ("pid", Value::opt_int(self.daemon_pid.map(|pid| pid as i64))),
                    ("updated_at", Value::Int(self.updated_at as i64)),
                    (
                        "state_age_s",
                        Value::opt_int(self.state_age.map(|age| age as i64)),
                    ),
                    ("stale", Value::Bool(self.health() == Health::Stale)),
                    ("interval_s", Value::Int(self.settings.interval as i64)),
                    ("cycle_count", Value::Int(self.cycle_count as i64)),
                    (
                        "consecutive_failures",
                        Value::Int(self.consecutive_failures as i64),
                    ),
                    ("last_good_cycle", Value::Int(self.last_good_cycle as i64)),
                    ("last_error", Value::opt_str(self.last_error.clone())),
                    (
                        "result",
                        Value::str(match self.snapshot.as_ref() {
                            Some(_) => self.result.as_str(),
                            None => "none",
                        }),
                    ),
                    ("sensor_failure", Value::Bool(self.sensor_failure())),
                ]),
            ),
            (
                "settings",
                Value::object([
                    (
                        "schema_version",
                        Value::Int(self.settings.schema_version as i64),
                    ),
                    ("enabled", Value::Bool(self.settings.enabled)),
                    ("mode", Value::str(self.settings.mode.as_str())),
                    ("curve", Value::str(self.settings.profile.as_str())),
                    (
                        "temp_source",
                        Value::str(self.settings.temp_source.as_str()),
                    ),
                    ("manual_pwm", Value::Int(self.settings.manual_pwm as i64)),
                    ("interval", Value::Int(self.settings.interval as i64)),
                    ("hysteresis", Value::Int(self.settings.hysteresis as i64)),
                    ("down_delay", Value::Int(self.settings.down_delay as i64)),
                    ("start_pwm", Value::Int(self.settings.start_pwm as i64)),
                    (
                        "start_boost_ms",
                        Value::Int(self.settings.start_boost_ms as i64),
                    ),
                    ("fail_pwm", Value::Int(self.settings.fail_pwm as i64)),
                    ("curve_data", Value::str(self.settings.curve_spec())),
                    (
                        "curve_silent",
                        Value::str(self.settings.curve_silent.clone()),
                    ),
                    (
                        "curve_balanced",
                        Value::str(self.settings.curve_balanced.clone()),
                    ),
                    (
                        "curve_performance",
                        Value::str(self.settings.curve_performance.clone()),
                    ),
                    (
                        "curve_custom",
                        Value::str(self.settings.curve_custom.clone()),
                    ),
                    (
                        "curve_valid",
                        Value::Bool(self.settings.curve_failure().is_none()),
                    ),
                    ("curve_error", Value::opt_str(self.settings.curve_failure())),
                    ("kernel_only", Value::Bool(self.settings.is_kernel_only())),
                    ("issues", issues),
                    (
                        "policy_signature",
                        Value::str(self.settings.policy_signature()),
                    ),
                ]),
            ),
            (
                "fan",
                Value::object([
                    ("pwm_path", Value::opt_str(self.pwm_path.clone())),
                    (
                        "pwm_value",
                        Value::opt_int(self.pwm_value.map(|v| v as i64)),
                    ),
                    ("pwm_max", Value::opt_int(self.pwm_max.map(|v| v as i64))),
                    ("requested_pwm", Value::Int(self.requested_pwm as i64)),
                    ("target_pwm", Value::Int(self.target_pwm as i64)),
                    ("applied_pwm", Value::Int(self.applied_pwm as i64)),
                    (
                        "requested_percent",
                        Value::Int(percent_of_pwm(self.requested_pwm) as i64),
                    ),
                    (
                        "target_percent",
                        Value::Int(percent_of_pwm(self.target_pwm) as i64),
                    ),
                    (
                        "applied_percent",
                        Value::Int(percent_of_pwm(self.applied_pwm) as i64),
                    ),
                    ("rpm", Value::opt_int(self.rpm.map(|v| v as i64))),
                    ("rpm_available", Value::Bool(self.rpm.is_some())),
                    ("fan_path", Value::opt_str(self.fan_path.clone())),
                    ("reason", Value::str(self.reason.clone())),
                    (
                        "reason_code",
                        Value::str(self.reason.split('+').next().unwrap_or("")),
                    ),
                    (
                        "kernel_floor_applied",
                        Value::Bool(self.kernel_floor_applied),
                    ),
                    ("boost_applied", Value::Bool(self.boost_applied)),
                    ("down_since", Value::Int(self.down_since as i64)),
                    (
                        "down_delay_remaining_s",
                        Value::Int(self.down_delay_remaining() as i64),
                    ),
                ]),
            ),
            (
                "thermal",
                Value::object([
                    ("floor_pwm", Value::Int(self.thermal_floor as i64)),
                    (
                        "floor_percent",
                        Value::Int(percent_of_pwm(self.thermal_floor) as i64),
                    ),
                    ("state", Value::Int(self.thermal_state as i64)),
                    ("max_state", Value::Int(self.thermal_max_state as i64)),
                    ("owner", Value::str(self.thermal_owner.clone())),
                    ("basis", Value::str(self.thermal_basis.clone())),
                    (
                        "cooling_levels",
                        Value::array(
                            self.cooling_levels
                                .iter()
                                .map(|level| Value::Int(*level as i64))
                                .collect::<Vec<Value>>(),
                        ),
                    ),
                    (
                        "map_states",
                        Value::array(
                            self.map_states
                                .iter()
                                .map(|(state, trigger)| {
                                    Value::object([
                                        ("state", Value::Int(*state as i64)),
                                        ("trigger_c", Value::Int(*trigger as i64)),
                                    ])
                                })
                                .collect::<Vec<Value>>(),
                        ),
                    ),
                    (
                        "legacy_policy_pending",
                        Value::Bool(self.legacy_policy_pending),
                    ),
                    (
                        "protections",
                        Value::object([
                            (
                                "zone",
                                Value::opt_str(
                                    self.protection
                                        .zone
                                        .as_ref()
                                        .map(|zone| zone.path.display().to_string()),
                                ),
                            ),
                            (
                                "type",
                                Value::opt_str(
                                    self.protection.zone.as_ref().map(|zone| zone.kind.clone()),
                                ),
                            ),
                            (
                                "policy",
                                Value::opt_str(
                                    self.protection
                                        .zone
                                        .as_ref()
                                        .and_then(|zone| zone.policy.clone()),
                                ),
                            ),
                            (
                                "available_policies",
                                Value::array(
                                    self.protection
                                        .zone
                                        .as_ref()
                                        .map(|zone| {
                                            zone.available_policies
                                                .iter()
                                                .map(|name| Value::str(name.clone()))
                                                .collect::<Vec<Value>>()
                                        })
                                        .unwrap_or_default(),
                                ),
                            ),
                            ("trips", trips),
                            (
                                "cpufreq_cooling",
                                Value::Bool(self.protection.cpufreq_cooling),
                            ),
                            (
                                "fan_cooling_device",
                                Value::opt_str(self.protection.fan_cooling_device.clone()),
                            ),
                            (
                                "fan_max_state",
                                Value::Int(self.protection.fan_max_state as i64),
                            ),
                            ("intact", Value::Bool(self.protection.protections_intact())),
                            (
                                "governor_hijacked",
                                Value::Bool(
                                    self.protection
                                        .zone
                                        .as_ref()
                                        .map(|zone| zone.governor_is_hijacked())
                                        .unwrap_or(false),
                                ),
                            ),
                            (
                                "warnings",
                                Value::array(
                                    self.protection
                                        .warnings
                                        .iter()
                                        .map(|warning| Value::str(warning.clone()))
                                        .collect::<Vec<Value>>(),
                                ),
                            ),
                        ]),
                    ),
                ]),
            ),
            (
                "control",
                Value::object([
                    ("temp_c", Value::opt_int(self.control_temp)),
                    ("sensor_label", Value::opt_str(self.control_sensor.clone())),
                    ("sensor_source", Value::opt_str(self.control_source.clone())),
                    (
                        "source_mode",
                        Value::str(self.settings.temp_source.as_str()),
                    ),
                ]),
            ),
            ("sensors", sensors),
            ("events", Value::array(events::to_values(&self.events))),
            (
                "paths",
                Value::object([
                    (
                        "uci_config",
                        Value::str(paths.uci_config.display().to_string()),
                    ),
                    (
                        "state",
                        Value::str(paths.state_file().display().to_string()),
                    ),
                    (
                        "event_log",
                        Value::str(paths.event_log().display().to_string()),
                    ),
                    ("lock_dir", Value::str(paths.lock_dir.display().to_string())),
                    ("sysfs", Value::str(paths.sysfs.display().to_string())),
                ]),
            ),
        ])
    }

    /// 2.x compatible key=value output, with the new keys appended.
    pub fn to_plain(&self) -> String {
        let mut out = String::with_capacity(2048);
        let mut line = |key: &str, value: String| {
            out.push_str(key);
            out.push('=');
            out.push_str(&value.replace(['\n', '\r'], " "));
            out.push('\n');
        };
        let setting = &self.settings;
        line("schema_version", setting.schema_version.to_string());
        line("enabled", u8::from(setting.enabled).to_string());
        line("mode", setting.mode.as_str().to_string());
        line("curve", setting.profile.as_str().to_string());
        line("curve_data", setting.curve_spec().to_string());
        line("temp_source", setting.temp_source.as_str().to_string());
        line("manual_pwm", setting.manual_pwm.to_string());
        line("interval", setting.interval.to_string());
        line("hysteresis", setting.hysteresis.to_string());
        line("down_delay", setting.down_delay.to_string());
        line("start_pwm", setting.start_pwm.to_string());
        line("start_boost_ms", setting.start_boost_ms.to_string());
        line("fail_pwm", setting.fail_pwm.to_string());
        line(
            "curve_valid",
            u8::from(setting.curve_failure().is_none()).to_string(),
        );
        line("pwm", show(self.pwm_path.clone()));
        line("pwm_value", show(self.pwm_value));
        line("pwm_max", show(self.pwm_max));
        line("fan", show(self.fan_path.clone()));
        line(
            "fan_feedback",
            u8::from(self.fan_path.is_some()).to_string(),
        );
        line("fan_rpm", show(self.rpm));
        for id in SensorId::all() {
            let Some(reading) = self.sensors.iter().find(|r| r.id == id) else {
                continue;
            };
            let prefix = id.key();
            line(&format!("{prefix}_temp"), show(reading.value_c));
            line(&format!("{prefix}_label"), show(reading.label.clone()));
            line(
                &format!("{prefix}_state"),
                reading.state.as_str().to_string(),
            );
            line(&format!("{prefix}_source"), show(reading.source.clone()));
            line(&format!("{prefix}_path"), show(reading.path.clone()));
            line(&format!("{prefix}_error"), show(reading.error.clone()));
            line(&format!("{prefix}_at"), reading.sampled_at.to_string());
        }
        let module_label = if self
            .sensors
            .iter()
            .any(|r| r.id == SensorId::Module && r.is_valid())
        {
            "5G modem".to_string()
        } else {
            String::new()
        };
        line("module_label", module_label);
        line("control_temp", show(self.control_temp));
        line("control_sensor", show(self.control_sensor.clone()));
        line("thermal_floor", self.thermal_floor.to_string());
        line("thermal_state", self.thermal_state.to_string());
        line("thermal_max_state", self.thermal_max_state.to_string());
        line("thermal_owner", self.thermal_owner.clone());
        line("thermal_basis", self.thermal_basis.clone());
        line("result", self.result.as_str().to_string());
        line("reason", self.reason.clone());
        line("requested_pwm", self.requested_pwm.to_string());
        line("target_pwm", self.target_pwm.to_string());
        line("applied_pwm", self.applied_pwm.to_string());
        line(
            "kernel_floor_applied",
            u8::from(self.kernel_floor_applied).to_string(),
        );
        line("boost_applied", u8::from(self.boost_applied).to_string());
        line("down_since", self.down_since.to_string());
        line(
            "down_delay_remaining",
            self.down_delay_remaining().to_string(),
        );
        line("state_age", show(self.state_age));
        line("controller_health", self.health().as_str().to_string());
        line("controller_version", crate::VERSION.to_string());
        line("controller_pid", show(self.daemon_pid));
        line("cycle_count", self.cycle_count.to_string());
        line(
            "consecutive_failures",
            self.consecutive_failures.to_string(),
        );
        line("last_good_cycle", self.last_good_cycle.to_string());
        line("last_error", show(self.last_error.clone()));
        line(
            "cpu_policy",
            show(self.protection.zone.as_ref().and_then(|z| z.policy.clone())),
        );
        line(
            "kernel_protections_intact",
            u8::from(self.protection.protections_intact()).to_string(),
        );
        line("sample_mode", self.mode.as_str().to_string());
        out
    }
}

fn readings_from_sensors(sensors: &Sensors) -> Vec<SensorReading> {
    sensors.readings.clone()
}

fn readings_from_snapshot(snapshot: &Snapshot) -> Vec<SensorReading> {
    if snapshot.sensors.is_empty() {
        return SensorId::all()
            .iter()
            .map(|id| SensorReading::new(*id, snapshot.updated))
            .collect();
    }
    snapshot.sensors.clone()
}

/// Assemble a status report.
///
/// With [`SampleMode::Cached`] this reads the published snapshot plus the two
/// registers the operator cares about most (PWM and RPM), so a browser polling
/// every few seconds cannot amplify into repeated hardware probes.
pub fn build(
    paths: &Paths,
    probe: &dyn RadioProbe,
    mode: SampleMode,
    now: u64,
) -> crate::Result<Report> {
    let settings = Settings::load(paths)?;
    let snapshot = Snapshot::read(paths);
    let device = fan::discover(paths);

    let mut report = Report {
        now,
        mode,
        settings: settings.clone(),
        snapshot: snapshot.clone(),
        sensors: Vec::new(),
        control_temp: None,
        control_sensor: None,
        control_source: None,
        pwm_path: device.as_ref().map(|d| d.pwm_path.display().to_string()),
        pwm_value: device.as_ref().and_then(|d| d.read_pwm_raw()),
        pwm_max: device.as_ref().and_then(|d| d.pwm_max()),
        rpm: device.as_ref().and_then(|d| d.read_rpm()),
        fan_path: device
            .as_ref()
            .and_then(|d| d.rpm_path.as_ref())
            .map(|p| p.display().to_string()),
        thermal_floor: 0,
        thermal_state: 0,
        thermal_max_state: 0,
        thermal_owner: "unknown".to_string(),
        thermal_basis: String::new(),
        cooling_levels: Vec::new(),
        map_states: Vec::new(),
        protection: safety::inspect_kernel_protection(paths, device.as_ref()),
        legacy_policy_pending: paths.legacy_policy_file().exists(),
        events: events::read_recent(paths, EVENT_LIMIT),
        daemon_pid: crate::lock::Lock::holder(paths),
        requested_pwm: 0,
        target_pwm: 0,
        applied_pwm: 0,
        reason: String::new(),
        kernel_floor_applied: false,
        boost_applied: false,
        down_since: 0,
        result: ControlResult::Error,
        last_error: None,
        cycle_count: 0,
        consecutive_failures: 0,
        last_good_cycle: 0,
        updated_at: 0,
        state_age: None,
        published: snapshot.is_some(),
    };

    // Whatever a control cycle published is reported in both modes: the reason,
    // the requested and applied output and the cycle counters describe the
    // controller, not the sample.
    if let Some(snapshot) = snapshot.as_ref() {
        report.requested_pwm = snapshot.requested_pwm;
        report.target_pwm = snapshot.target_pwm;
        report.applied_pwm = snapshot.applied_pwm;
        report.reason = snapshot.reason.clone();
        report.kernel_floor_applied = snapshot.kernel_floor_applied;
        report.boost_applied = snapshot.boost_applied;
        report.down_since = snapshot.down_since;
        report.result = snapshot.result;
        report.last_error = snapshot.last_error.clone();
        report.cycle_count = snapshot.cycle_count;
        report.consecutive_failures = snapshot.consecutive_failures;
        report.last_good_cycle = snapshot.last_good_cycle;
        report.updated_at = snapshot.updated;
        report.state_age = snapshot.age(now);
        report.thermal_floor = snapshot.thermal_floor;
        report.thermal_state = snapshot.thermal_state;
        report.thermal_max_state = snapshot.thermal_max_state;
        report.thermal_owner = snapshot.thermal_owner.clone();
        report.thermal_basis = snapshot.thermal_basis.clone();
        report.control_temp = snapshot.control_temp;
        report.control_sensor = snapshot.control_sensor.clone();
        report.control_source = snapshot.control_source.clone();
        report.sensors = readings_from_snapshot(snapshot);
    }

    if mode == SampleMode::Live {
        let sensors = crate::sensor::sample_all(paths, probe, now);
        report.sensors = readings_from_sensors(&sensors);
        if let Some((reading, value)) = sensors.control(&settings) {
            report.control_temp = Some(value);
            report.control_sensor = reading.label.clone();
            report.control_source = reading.source.clone();
        }
        if let Some(device) = device.as_ref() {
            let floor = safety::evaluate_floor(paths, device, sensors.value(SensorId::Cpu));
            report.thermal_floor = floor.floor_pwm;
            report.thermal_state = floor.thermal_state;
            report.thermal_max_state = floor.max_state;
            report.thermal_owner = if settings.is_kernel_only() {
                Ownership::Kernel.as_str().to_string()
            } else {
                floor.ownership.as_apply_str().to_string()
            };
            report.thermal_basis = floor.basis;
            report.cooling_levels = floor.cooling_levels;
            report.map_states = floor.map_states;
        } else {
            report.thermal_basis = "no pwm control node found".to_string();
        }
    }

    if report.sensors.is_empty() {
        // Never omit a sensor slot: an absent row would let a client invent a
        // default, while an explicit "missing" row says what it means.
        report.sensors = crate::controller::placeholder_readings(now);
    }
    if !report.published {
        // Nothing was ever published, so the register is all we have; with a
        // snapshot its `applied_pwm` stays authoritative and `fan.pwm_value`
        // carries the live register reading shown as the actual output.
        if let Some(value) = report.pwm_value {
            report.applied_pwm = value.min(255) as u8;
        }
    }
    if report.reason.is_empty() {
        report.reason = if device.is_none() {
            "pwm-missing".to_string()
        } else {
            "no-snapshot".to_string()
        };
    }
    if device.is_none() {
        // Without a control node the controller cannot cool anything, so a
        // healthy cached snapshot must not keep reporting "running".
        report.result = ControlResult::Error;
        report.reason = "pwm-missing".to_string();
        report.last_error = Some("pwm control node not found".to_string());
    }
    Ok(report)
}

/// Field list of the JSON contract, used by `--schema` and by CI.
pub fn contract() -> Vec<String> {
    let paths = Paths::default();
    let probe = crate::sensor::IwprivProbe::default();
    // Build against a sandbox that does not exist: every optional field is then
    // null and the writer still enumerates the full key set.
    let mut report = build(
        &Paths::sandbox("/nonexistent-h5000m"),
        &probe,
        SampleMode::Cached,
        0,
    )
    .expect("a report against missing paths is still structurally valid");
    // Arrays only enumerate their members when they hold one, so seed a
    // placeholder event and trip: the contract then lists every nested field
    // even on a machine that never produced one.
    if report.events.is_empty() {
        report.events.push(events::Event::new(
            events::Level::Info,
            "contract",
            "placeholder",
            0,
        ));
    }
    if report.protection.trips.is_empty() {
        report.protection.trips.push(safety::Trip {
            zone: String::new(),
            name: String::new(),
            kind: String::new(),
            temp_c: None,
            hysteresis_c: 0,
        });
    }
    let value = report.to_value(&paths);
    let mut paths = value.paths();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_rounding_matches_the_frontend() {
        assert_eq!(percent_of_pwm(0), 0);
        assert_eq!(percent_of_pwm(255), 100);
        assert_eq!(percent_of_pwm(128), 50);
        assert_eq!(percent_of_pwm(1), 0);
        assert_eq!(percent_of_pwm(2), 1);
    }

    #[test]
    fn health_prefers_error_then_stale_then_failsafe() {
        let mut snapshot = Snapshot::published_now(1000);
        snapshot.result = ControlResult::Ok;
        assert_eq!(compute_health(Some(&snapshot), 1002, 5), Health::Running);
        snapshot.result = ControlResult::Failsafe;
        assert_eq!(compute_health(Some(&snapshot), 1002, 5), Health::Failsafe);
        snapshot.result = ControlResult::Error;
        assert_eq!(compute_health(Some(&snapshot), 1002, 5), Health::Error);
        snapshot.result = ControlResult::Ok;
        assert_eq!(compute_health(Some(&snapshot), 2000, 5), Health::Stale);
        assert_eq!(compute_health(None, 2000, 5), Health::Stopped);
    }

    #[test]
    fn down_delay_remaining_is_only_reported_for_a_hold() {
        let mut report = empty_report();
        report.reason = "auto-down-delay".to_string();
        report.down_since = 1000;
        report.now = 1010;
        report.settings.down_delay = 30;
        assert_eq!(report.down_delay_remaining(), 20);
        report.reason = "auto".to_string();
        assert_eq!(report.down_delay_remaining(), 0);
        report.reason = "auto-down-delay".to_string();
        report.down_since = 0;
        assert_eq!(report.down_delay_remaining(), 0);
    }

    #[test]
    fn unknown_values_are_null_in_json_and_empty_in_plain() {
        let report = empty_report();
        let json = report
            .to_value(&Paths::sandbox("/nonexistent-h5000m"))
            .to_json();
        assert!(json.contains("\"value_c\":null"), "{json}");
        assert!(
            !json.contains("\"value_c\":0,"),
            "zero must not fake a reading"
        );
        let plain = report.to_plain();
        assert!(plain.contains("cpu_temp=\n"), "{plain}");
        assert!(plain.contains("control_temp=\n"));
        assert!(plain.contains("fan_rpm=\n"));
    }

    #[test]
    fn plain_output_keeps_every_legacy_key() {
        let plain = empty_report().to_plain();
        for key in [
            "enabled=",
            "mode=",
            "curve=",
            "curve_data=",
            "temp_source=",
            "manual_pwm=",
            "interval=",
            "hysteresis=",
            "down_delay=",
            "start_pwm=",
            "start_boost_ms=",
            "pwm=",
            "pwm_value=",
            "fan=",
            "fan_feedback=",
            "fan_rpm=",
            "cpu_temp=",
            "cpu_label=",
            "phy_temp=",
            "phy_label=",
            "wifi1_temp=",
            "wifi1_label=",
            "wifi2_temp=",
            "wifi2_label=",
            "module_temp=",
            "module_label=",
            "control_temp=",
            "control_sensor=",
            "thermal_floor=",
            "thermal_state=",
            "thermal_max_state=",
            "thermal_owner=",
            "result=",
            "reason=",
            "requested_pwm=",
            "target_pwm=",
            "applied_pwm=",
            "state_age=",
        ] {
            assert!(plain.contains(key), "missing legacy key {key}");
        }
    }

    #[test]
    fn json_paths_are_reported_for_every_sensor() {
        let report = empty_report();
        let json = report.to_value(&Paths::sandbox("/x")).to_json();
        for id in ["cpu", "phy", "wifi1", "wifi2", "module"] {
            let needle = format!(r#""id":"{id}""#);
            assert!(json.contains(&needle), "sensor {id} missing");
        }
    }

    fn empty_report() -> Report {
        let probe = crate::sensor::IwprivProbe::default();
        build(
            &Paths::sandbox("/nonexistent-h5000m"),
            &probe,
            SampleMode::Cached,
            1000,
        )
        .expect("a report is always constructible")
    }

    #[test]
    fn contract_lists_nested_fields() {
        let paths = contract();
        assert!(paths.contains(&"fan.target_pwm".to_string()));
        assert!(paths.contains(&"sensors[].value_c".to_string()));
        assert!(paths.contains(&"thermal.protections.intact".to_string()));
        assert!(paths.contains(&"controller.health".to_string()));
        assert!(paths.iter().any(|path| path.starts_with("settings.")));
    }

    #[test]
    fn sensor_failure_is_reported_when_nothing_is_valid() {
        let report = empty_report();
        assert!(report.sensor_failure());
        assert_eq!(report.health(), Health::Stopped);
        assert_eq!(report.result, ControlResult::Error);
        assert_eq!(report.reason, "pwm-missing");
    }

    #[test]
    fn config_issues_are_exposed_for_the_diagnostics_tab() {
        let mut report = empty_report();
        report.settings.issues.push(crate::config::ConfigIssue {
            option: "interval".to_string(),
            value: "900".to_string(),
            problem: "above the maximum of 60".to_string(),
            applied: "clamped to 60".to_string(),
        });
        let json = report.to_value(&Paths::sandbox("/x")).to_json();
        assert!(json.contains("\"option\":\"interval\""));
        assert!(json.contains("clamped to 60"));
    }

    #[test]
    fn stale_sensor_state_is_kept_as_stale() {
        let mut snapshot = Snapshot::published_now(1000);
        snapshot.result = ControlResult::Ok;
        snapshot.sensors = vec![SensorReading::new(SensorId::Module, 900)
            .with_problem(crate::sensor::SensorState::Stale, "100 s old".to_string())];
        let decoded = Snapshot::decode(&snapshot.encode());
        let module = decoded
            .sensor(SensorId::Module)
            .expect("the module slot is always present");
        assert_eq!(module.state, crate::sensor::SensorState::Stale);
        assert_eq!(module.value_c, None);
    }
}
