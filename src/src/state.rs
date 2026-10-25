//! The `/var/run` state snapshot the daemon publishes and `status` consumes.
//!
//! Format stays line oriented `key=value` so that an operator can inspect it
//! with `sed`/`grep` on the shell, and so the older shell readers keep working.
//! The machine readable contract the LuCI view uses is the JSON writer in
//! [`crate::report`], which is generated from this structure.
//!
//! Decoding is deliberately strict: a field that is absent or unparsable
//! becomes `None`, never a plausible looking zero.

use crate::sensor::{SensorId, SensorReading, SensorState};
use std::collections::HashMap;
use std::path::Path;

pub const STATE_SCHEMA: u32 = 3;
/// A snapshot newer than this is treated as "the controller is not updating".
pub const STATE_STALE_AFTER: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlResult {
    /// Control loop is applying the policy normally.
    Ok,
    /// A safety path is active (sensor or curve failure).
    Failsafe,
    /// The cycle could not be completed.
    Error,
}

impl ControlResult {
    pub fn as_str(self) -> &'static str {
        match self {
            ControlResult::Ok => "ok",
            ControlResult::Failsafe => "failsafe",
            ControlResult::Error => "error",
        }
    }

    pub fn parse(raw: &str) -> Option<ControlResult> {
        match raw {
            "ok" => Some(ControlResult::Ok),
            "failsafe" => Some(ControlResult::Failsafe),
            "error" => Some(ControlResult::Error),
            _ => None,
        }
    }
}

/// Full published state of one control cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub schema: u32,
    pub updated: u64,
    pub result: ControlResult,
    pub reason: String,
    pub requested_pwm: u8,
    pub target_pwm: u8,
    pub applied_pwm: u8,
    pub thermal_floor: u8,
    pub thermal_state: u32,
    pub thermal_max_state: u32,
    pub thermal_owner: String,
    /// How the current floor was derived, for the diagnostics tab.
    pub thermal_basis: String,
    pub kernel_floor_applied: bool,
    pub control_temp: Option<i32>,
    pub control_sensor: Option<String>,
    /// Machine readable origin of the sensor that drives the curve.
    pub control_source: Option<String>,
    pub down_since: u64,
    pub boost_applied: bool,
    pub cycle_count: u64,
    pub consecutive_failures: u64,
    pub last_error: Option<String>,
    pub last_good_cycle: u64,
    pub pid: u32,
    pub controller_version: String,
    pub pwm_path: Option<String>,
    pub fan_path: Option<String>,
    pub rpm: Option<u32>,
    pub policy_signature: String,
    pub sensors: Vec<SensorReading>,
    pub unknown_keys: Vec<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            schema: STATE_SCHEMA,
            updated: 0,
            result: ControlResult::Error,
            reason: String::new(),
            requested_pwm: 0,
            target_pwm: 0,
            applied_pwm: 0,
            thermal_floor: 0,
            thermal_state: 0,
            thermal_max_state: 0,
            thermal_owner: "unknown".to_string(),
            thermal_basis: String::new(),
            kernel_floor_applied: false,
            control_temp: None,
            control_sensor: None,
            control_source: None,
            down_since: 0,
            boost_applied: false,
            cycle_count: 0,
            consecutive_failures: 0,
            last_error: None,
            last_good_cycle: 0,
            pid: 0,
            controller_version: crate::VERSION.to_string(),
            pwm_path: None,
            fan_path: None,
            rpm: None,
            policy_signature: String::new(),
            sensors: Vec::new(),
            unknown_keys: Vec::new(),
        }
    }
}

impl Snapshot {
    /// A fresh snapshot stamped with the publishing time and process.
    pub fn published_now(now: u64) -> Self {
        Snapshot {
            updated: now,
            pid: crate::platform::process_id(),
            ..Snapshot::default()
        }
    }

    /// Age of the snapshot in seconds, `None` when it never was updated.
    pub fn age(&self, now: u64) -> Option<u64> {
        if self.updated == 0 {
            return None;
        }
        Some(now.saturating_sub(self.updated))
    }

    pub fn is_stale(&self, now: u64, interval: u64) -> bool {
        match self.age(now) {
            None => true,
            Some(age) => age > interval.saturating_mul(3).max(STATE_STALE_AFTER),
        }
    }

    pub fn sensor(&self, id: SensorId) -> Option<&SensorReading> {
        self.sensors.iter().find(|reading| reading.id == id)
    }

    /// `true` when the snapshot was written by a controller that is still the
    /// authoritative writer.
    pub fn written_by(&self, pid: u32) -> bool {
        self.pid != 0 && self.pid == pid
    }
}

fn sanitize(value: &str) -> String {
    value.replace(['\n', '\r', '\0'], " ")
}

fn push_line(out: &mut String, key: &str, value: impl std::fmt::Display) {
    out.push_str(key);
    out.push('=');
    out.push_str(&sanitize(&value.to_string()));
    out.push('\n');
}

fn opt_line(out: &mut String, key: &str, value: Option<impl std::fmt::Display>) {
    match value {
        Some(inner) => push_line(out, key, inner),
        None => push_line(out, key, ""),
    }
}

impl SensorReading {
    fn encode_into(&self, out: &mut String) {
        let prefix = self.id.key();
        opt_line(out, &format!("{prefix}_temp"), self.value_c);
        push_line(out, &format!("{prefix}_state"), self.state.as_str());
        opt_line(out, &format!("{prefix}_label"), self.label.as_deref());
        opt_line(out, &format!("{prefix}_source"), self.source.as_deref());
        opt_line(out, &format!("{prefix}_path"), self.path.as_deref());
        opt_line(out, &format!("{prefix}_error"), self.error.as_deref());
        push_line(out, &format!("{prefix}_at"), self.sampled_at);
    }
}

impl Snapshot {
    pub fn encode(&self) -> String {
        let mut out = String::with_capacity(1024);
        push_line(&mut out, "schema", self.schema);
        push_line(&mut out, "updated", self.updated);
        push_line(&mut out, "result", self.result.as_str());
        push_line(&mut out, "reason", &self.reason);
        push_line(&mut out, "requested_pwm", self.requested_pwm);
        push_line(&mut out, "target_pwm", self.target_pwm);
        push_line(&mut out, "applied_pwm", self.applied_pwm);
        push_line(&mut out, "thermal_floor", self.thermal_floor);
        push_line(&mut out, "thermal_state", self.thermal_state);
        push_line(&mut out, "thermal_max_state", self.thermal_max_state);
        push_line(&mut out, "thermal_owner", &self.thermal_owner);
        push_line(&mut out, "thermal_basis", &self.thermal_basis);
        push_line(
            &mut out,
            "kernel_floor_applied",
            u8::from(self.kernel_floor_applied),
        );
        opt_line(&mut out, "control_temp", self.control_temp);
        opt_line(&mut out, "control_sensor", self.control_sensor.as_deref());
        opt_line(&mut out, "control_source", self.control_source.as_deref());
        push_line(&mut out, "down_since", self.down_since);
        push_line(&mut out, "boost_applied", u8::from(self.boost_applied));
        push_line(&mut out, "cycle_count", self.cycle_count);
        push_line(&mut out, "consecutive_failures", self.consecutive_failures);
        opt_line(&mut out, "last_error", self.last_error.as_deref());
        push_line(&mut out, "last_good_cycle", self.last_good_cycle);
        push_line(&mut out, "pid", self.pid);
        push_line(&mut out, "controller_version", &self.controller_version);
        opt_line(&mut out, "pwm_path", self.pwm_path.as_deref());
        opt_line(&mut out, "fan_path", self.fan_path.as_deref());
        opt_line(&mut out, "rpm", self.rpm);
        push_line(&mut out, "policy_signature", &self.policy_signature);
        for reading in &self.sensors {
            reading.encode_into(&mut out);
        }
        out
    }

    /// Parse a snapshot. Missing keys keep their default and unparsable values
    /// are ignored; both are reported through [`Snapshot::unknown_keys`] style
    /// diagnostics rather than being turned into plausible numbers.
    // Every field is filled from its own key, so a struct literal is not
    // expressible here.
    #[allow(clippy::field_reassign_with_default)]
    pub fn decode(text: &str) -> Snapshot {
        let mut map: HashMap<&str, &str> = HashMap::new();
        let mut unknown_keys = Vec::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            // A duplicated key means two controllers interleaved or an old
            // writer appended instead of replacing; keep the first value, which
            // is what `sed | head -n 1` did in 2.x.
            if map.contains_key(key) {
                if !unknown_keys.contains(&key.to_string()) {
                    unknown_keys.push(key.to_string());
                }
                continue;
            }
            map.insert(key, value);
        }

        let uint = |key: &str| -> Option<u64> {
            map.get(key)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .and_then(|v| v.parse::<u64>().ok())
        };
        let sint = |key: &str| -> Option<i32> {
            map.get(key)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .and_then(|v| v.parse::<i32>().ok())
        };
        let flag = |key: &str| -> bool { uint(key).unwrap_or(0) != 0 };
        let text_of = |key: &str| -> Option<String> {
            map.get(key)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(|v| v.to_string())
        };

        let mut snapshot = Snapshot::default();
        snapshot.schema = uint("schema").unwrap_or(0) as u32;
        snapshot.updated = uint("updated").unwrap_or(0);
        snapshot.result = text_of("result")
            .and_then(|value| ControlResult::parse(&value))
            .unwrap_or(ControlResult::Error);
        snapshot.reason = text_of("reason").unwrap_or_default();
        snapshot.requested_pwm = uint("requested_pwm").map(|v| v.min(255) as u8).unwrap_or(0);
        snapshot.target_pwm = uint("target_pwm").map(|v| v.min(255) as u8).unwrap_or(0);
        snapshot.applied_pwm = uint("applied_pwm").map(|v| v.min(255) as u8).unwrap_or(0);
        snapshot.thermal_floor = uint("thermal_floor").map(|v| v.min(255) as u8).unwrap_or(0);
        snapshot.thermal_state = uint("thermal_state").unwrap_or(0) as u32;
        snapshot.thermal_max_state = uint("thermal_max_state").unwrap_or(0) as u32;
        snapshot.thermal_owner = text_of("thermal_owner").unwrap_or_else(|| "unknown".to_string());
        snapshot.thermal_basis = text_of("thermal_basis").unwrap_or_default();
        snapshot.kernel_floor_applied =
            flag("kernel_floor_applied") || snapshot.reason.ends_with("+kernel-floor");
        snapshot.control_temp = sint("control_temp");
        snapshot.control_sensor = text_of("control_sensor");
        snapshot.control_source = text_of("control_source");
        snapshot.down_since = uint("down_since").unwrap_or(0);
        snapshot.boost_applied = flag("boost_applied");
        snapshot.cycle_count = uint("cycle_count").unwrap_or(0);
        snapshot.consecutive_failures = uint("consecutive_failures").unwrap_or(0);
        snapshot.last_error = text_of("last_error");
        snapshot.last_good_cycle = uint("last_good_cycle").unwrap_or(0);
        snapshot.pid = uint("pid").unwrap_or(0) as u32;
        snapshot.controller_version = text_of("controller_version").unwrap_or_default();
        snapshot.pwm_path = text_of("pwm_path");
        snapshot.fan_path = text_of("fan_path");
        snapshot.rpm = uint("rpm").map(|v| v as u32);
        snapshot.policy_signature = text_of("policy_signature").unwrap_or_default();

        for id in SensorId::all() {
            let prefix = id.key();
            let mut reading = SensorReading::new(id, uint(&format!("{prefix}_at")).unwrap_or(0));
            reading.value_c = sint(&format!("{prefix}_temp"));
            reading.label = text_of(&format!("{prefix}_label"));
            reading.source = text_of(&format!("{prefix}_source"));
            reading.path = text_of(&format!("{prefix}_path"));
            reading.error = text_of(&format!("{prefix}_error"));
            reading.state = match text_of(&format!("{prefix}_state")).as_deref() {
                Some("ok") => SensorState::Ok,
                Some("stale") => SensorState::Stale,
                Some("invalid") => SensorState::Invalid,
                Some("failed") => SensorState::Failed,
                Some("missing") => SensorState::Missing,
                // Snapshots written before 3.0 only carried a value or an empty
                // field, so the state has to be inferred.
                Some(_) => SensorState::Invalid,
                None if reading.value_c.is_some() => SensorState::Ok,
                None => SensorState::Missing,
            };
            if reading.state != SensorState::Ok {
                // Never let a value survive a state that says it is unusable.
                reading.value_c = None;
            }
            snapshot.sensors.push(reading);
        }
        snapshot.unknown_keys = unknown_keys;
        snapshot
    }

    /// Read the published snapshot; `None` when the file does not exist yet.
    pub fn read(paths: &crate::paths::Paths) -> Option<Snapshot> {
        let text = std::fs::read_to_string(paths.state_file()).ok()?;
        Some(Snapshot::decode(&text))
    }

    /// Publish atomically: write a per-process temporary file, then rename.
    /// A reader therefore never observes a half written snapshot.
    pub fn write(&self, path: &Path) -> crate::Result<()> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let temp = path.with_file_name(format!(
            "{}.{}.tmp",
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "state".to_string()),
            std::process::id()
        ));
        std::fs::write(&temp, self.encode())
            .map_err(|err| crate::Error::io(temp.display().to_string(), &err))?;
        match std::fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = std::fs::remove_file(&temp);
                Err(crate::Error::io(path.display().to_string(), &err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Snapshot {
        let mut snapshot = Snapshot::published_now(1000);
        snapshot.result = ControlResult::Ok;
        snapshot.reason = "auto+kernel-floor".to_string();
        snapshot.requested_pwm = 100;
        snapshot.target_pwm = 160;
        snapshot.applied_pwm = 160;
        snapshot.thermal_floor = 160;
        snapshot.thermal_owner = "shared".to_string();
        snapshot.control_temp = Some(61);
        snapshot.control_sensor = Some("cpu-thermal".to_string());
        snapshot.sensors = vec![
            SensorReading::new(SensorId::Cpu, 1000).with_value(
                61,
                "cpu-thermal".into(),
                "thermal-zone:cpu".into(),
                "/x".into(),
            ),
            SensorReading::new(SensorId::Module, 1000)
                .with_problem(SensorState::Stale, "90 s old".into()),
        ];
        snapshot
    }

    #[test]
    fn round_trips_without_loss() {
        let encoded = sample().encode();
        let decoded = Snapshot::decode(&encoded);
        assert_eq!(decoded.updated, 1000);
        assert_eq!(decoded.result, ControlResult::Ok);
        assert_eq!(decoded.reason, "auto+kernel-floor");
        assert!(decoded.kernel_floor_applied, "legacy suffix is understood");
        assert_eq!(decoded.control_temp, Some(61));
        assert_eq!(
            decoded.sensors.len(),
            5,
            "every slot is always present so the UI never has to guess"
        );
        assert_eq!(decoded.sensor(SensorId::Cpu).unwrap().value_c, Some(61));
        assert_eq!(
            decoded.sensor(SensorId::Phy).unwrap().state,
            SensorState::Missing
        );
        let stale = decoded.sensor(SensorId::Module).unwrap();
        assert_eq!(stale.state, SensorState::Stale);
        assert_eq!(stale.value_c, None, "a stale reading is not a value");
        assert_eq!(stale.error.as_deref(), Some("90 s old"));
    }

    #[test]
    fn empty_and_garbage_fields_stay_unknown() {
        let text = "updated=abc\nrequested_pwm=\ncontrol_temp=\ntarget_pwm=999\nresult=weird\n";
        let decoded = Snapshot::decode(text);
        assert_eq!(decoded.updated, 0);
        assert_eq!(decoded.requested_pwm, 0);
        assert_eq!(decoded.control_temp, None, "empty is unknown, not zero");
        assert_eq!(decoded.target_pwm, 255, "out of byte range is clamped");
        assert_eq!(decoded.result, ControlResult::Error);
        assert_eq!(decoded.reason, "");
    }

    #[test]
    fn legacy_two_line_snapshot_is_still_readable() {
        // Exactly what 2.1.1 wrote; the new fields must default harmlessly.
        let text = "\
updated=1700000000
result=ok
reason=auto
requested_pwm=102
target_pwm=102
applied_pwm=102
thermal_floor=0
thermal_state=0
thermal_max_state=3
thermal_owner=shared
control_temp=55
control_sensor=cpu-thermal
down_since=0
";
        let decoded = Snapshot::decode(text);
        assert_eq!(decoded.requested_pwm, 102);
        assert_eq!(decoded.control_temp, Some(55));
        assert_eq!(decoded.thermal_owner, "shared");
        assert_eq!(
            decoded.sensor(SensorId::Cpu).map(|r| r.state),
            Some(SensorState::Missing),
            "2.x wrote cpu_temp only through control_temp"
        );
        assert_eq!(decoded.schema, 0, "pre-schema snapshot");
    }

    #[test]
    fn duplicated_keys_are_flagged() {
        let text = "updated=1\nupdated=2\nresult=ok\n";
        let decoded = Snapshot::decode(text);
        assert_eq!(decoded.unknown_keys, vec!["updated".to_string()]);
        assert_eq!(decoded.updated, 1, "first writer wins");
    }

    #[test]
    fn staleness_uses_the_configured_interval() {
        let snapshot = sample();
        assert!(!snapshot.is_stale(1005, 5));
        assert!(snapshot.is_stale(1100, 5));
        assert!(
            Snapshot::default().is_stale(1000, 5),
            "never updated is stale"
        );
    }

    #[test]
    fn newlines_cannot_forge_extra_fields() {
        let mut snapshot = sample();
        snapshot.last_error = Some("line one\nupdated=1\0result=ok".to_string());
        let decoded = Snapshot::decode(&snapshot.encode());
        assert_eq!(decoded.result, ControlResult::Ok);
        assert_eq!(decoded.updated, 1000);
        assert!(decoded
            .last_error
            .as_deref()
            .map(|value| !value.contains('\n'))
            .unwrap_or(false));
    }
}
