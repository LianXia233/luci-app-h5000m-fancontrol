//! Kernel thermal state: who owns the fan, and what minimum cooling the kernel
//! still demands.
//!
//! Two invariants drive this module.
//!
//! 1. The controller only writes PWM. It never switches a CPU thermal zone to
//!    `user_space`, because that would detach the kernel's CPU throttling and
//!    hot/critical protection. The only policy write left is undoing what the
//!    2.0.x releases did (see [`restore_legacy_policy`]).
//! 2. When the kernel still drives the fan through a cooling map, the effective
//!    output can never be below what that map demands. `cur_state` is not used
//!    as the floor, because a direct PWM write changes `cur_state` too; the
//!    requirement is derived from the devicetree cooling maps instead.

use crate::fan::{self, FanDevice};
use crate::paths::{list_dirs, read_trimmed, Paths};
use std::path::{Path, PathBuf};

/// `pwm-fan` output levels are 8 bit on this board.
pub const PWM_LIMIT: u32 = 255;
/// Emergency map states of the stock H5000M device tree: 83 C raises the fan a
/// step, 113 C is the last state. These are the kernel's own trip points, not a
/// project invention, and the shell controller used the same numbers.
pub const EMERGENCY_STATE_2_C: i32 = 83;
pub const EMERGENCY_STATE_3_C: i32 = 113;
/// Conservative first active trip used when devicetree discovery fails on older
/// firmware that mapped 38 C to state 1.
pub const FALLBACK_STATE_1_C: i32 = 38;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// The fan is referenced by at least one cooling map, so the kernel and the
    /// controller both move the PWM output.
    Shared,
    /// Cooling maps exist but none reference the fan (the shipped
    /// `openwrt-patches` case): the controller owns the fan policy alone.
    Exclusive,
    /// The controller is disabled or in kernel-only mode.
    Kernel,
    /// Nothing could be determined; treated as controller owned.
    Unknown,
}

impl Ownership {
    pub fn as_str(self) -> &'static str {
        match self {
            Ownership::Shared => "shared",
            Ownership::Exclusive => "userspace",
            Ownership::Kernel => "kernel",
            Ownership::Unknown => "unknown",
        }
    }

    /// Value the 2.x controller published while applying a policy.
    pub fn as_apply_str(self) -> &'static str {
        match self {
            Ownership::Unknown => "userspace-pwm",
            other => other.as_str(),
        }
    }

    pub fn parse(raw: &str) -> Ownership {
        match raw {
            "shared" => Ownership::Shared,
            "userspace" | "userspace-pwm" => Ownership::Exclusive,
            "kernel" => Ownership::Kernel,
            _ => Ownership::Unknown,
        }
    }

    pub fn describes_kernel_protection_active(self) -> bool {
        matches!(self, Ownership::Shared | Ownership::Kernel)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trip {
    pub zone: String,
    pub name: String,
    pub kind: String,
    pub temp_c: Option<i32>,
    pub hysteresis_c: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuZone {
    pub path: PathBuf,
    pub kind: String,
    pub policy: Option<String>,
    pub available_policies: Vec<String>,
}

impl CpuZone {
    /// A `user_space` governor on the CPU zone means the kernel stopped managing
    /// it, which the controller must never cause and should surface loudly.
    pub fn governor_is_hijacked(&self) -> bool {
        self.policy.as_deref() == Some("user_space")
    }
}

/// Everything the diagnostics need about kernel side protection.
///
/// Linux does not expose trip points under `thermal_zoneN`, so they are read
/// from the devicetree where the firmware actually declares them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelProtection {
    pub zone: Option<CpuZone>,
    pub trips: Vec<Trip>,
    pub cpufreq_cooling: bool,
    pub fan_cooling_device: Option<String>,
    pub fan_max_state: u32,
    pub warnings: Vec<String>,
}

impl KernelProtection {
    pub fn critical_trip(&self) -> Option<&Trip> {
        self.trips.iter().find(|trip| trip.kind == "critical")
    }

    pub fn hot_trip(&self) -> Option<&Trip> {
        self.trips.iter().find(|trip| trip.kind == "hot")
    }

    pub fn passive_trip(&self) -> Option<&Trip> {
        self.trips.iter().find(|trip| trip.kind == "passive")
    }

    /// True when CPU throttling and at least one high temperature trip survive.
    pub fn protections_intact(&self) -> bool {
        let Some(zone) = self.zone.as_ref() else {
            return false;
        };
        !zone.governor_is_hijacked()
            && (self.critical_trip().is_some() || self.hot_trip().is_some())
    }
}

/// Read one big-endian u32 out of a devicetree property at 4-byte `index`.
pub fn dt_be32(path: &Path, index: usize) -> Option<u32> {
    let bytes = std::fs::read(path).ok()?;
    let offset = index.checked_mul(4)?;
    if bytes.len() < offset + 4 {
        return None;
    }
    let window = &bytes[offset..offset + 4];
    Some(u32::from_be_bytes([
        window[0], window[1], window[2], window[3],
    ]))
}

/// Locate the CPU thermal zone for inspection.
///
/// Preference follows the shell rule (a zone whose type mentions cpu, soc or
/// mt798), but the `user_space` requirement is dropped: reading a policy does
/// not need the zone to be switchable, and insisting on it would hide the
/// governor state on firmware where only `step_wise` is offered.
pub fn find_cpu_zone(paths: &Paths) -> Option<CpuZone> {
    let mut fallback: Option<CpuZone> = None;
    for zone in list_dirs(&paths.thermal_dir(), "thermal_zone") {
        let policy_path = zone.join("policy");
        let available_path = zone.join("available_policies");
        if !policy_path.exists() {
            continue;
        }
        let kind = read_trimmed(&zone.join("type")).unwrap_or_else(|| "unknown".to_string());
        let policy = read_trimmed(&policy_path);
        let available_policies: Vec<String> = read_trimmed(&available_path)
            .map(|text| {
                text.split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        let candidate = CpuZone {
            path: zone.clone(),
            kind: kind.clone(),
            policy,
            available_policies,
        };
        let lowered = kind.to_lowercase();
        if lowered.contains("cpu") || lowered.contains("soc") || lowered.contains("mt798") {
            return Some(candidate);
        }
        if fallback.is_none() {
            fallback = Some(candidate);
        }
    }
    fallback
}

/// All trips of every zone, used to resolve a cooling map trip phandle and to
/// report which protections the firmware still declares.
fn all_trips(paths: &Paths) -> Vec<(PathBuf, Trip)> {
    let mut out = Vec::new();
    for zone in list_dirs(&paths.cooling_map_root(), "") {
        for trip in list_dirs(&zone.join("trips"), "trip-point") {
            let name = read_trimmed(&trip.join("name"))
                .or_else(|| {
                    trip.file_name()
                        .map(|value| value.to_string_lossy().to_string())
                })
                .unwrap_or_else(|| "unknown".to_string());
            let kind = read_trimmed(&trip.join("type")).unwrap_or_else(|| "unknown".to_string());
            let temp_millic = dt_be32(&trip.join("temperature"), 0);
            let temp_c = temp_millic.map(|value| (value / 1000) as i32);
            let hysteresis_c = dt_be32(&trip.join("hysteresis"), 0)
                .map(|value| value / 1000)
                .unwrap_or(0);
            let zone_name = zone
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_default();
            out.push((
                trip,
                Trip {
                    zone: zone_name,
                    name,
                    kind,
                    temp_c,
                    hysteresis_c,
                },
            ));
        }
    }
    out
}

fn cooling_map_dirs(paths: &Paths) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for zone in list_dirs(&paths.cooling_map_root(), "") {
        out.extend(list_dirs(&zone.join("cooling-maps"), ""));
    }
    out
}

/// `pwm-fan` cooling level table, indexed by cooling state.
pub fn cooling_levels(of_dir: Option<&Path>, paths: &Paths) -> Vec<u32> {
    let candidate = match of_dir {
        Some(dir) => {
            let direct = dir.join("cooling-levels");
            if direct.is_file() {
                Some(direct)
            } else {
                None
            }
        }
        None => None,
    };
    let path = candidate.unwrap_or_else(|| paths.pwm_fan_dt().join("cooling-levels"));
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let mut levels = Vec::new();
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        levels.push(u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]));
        offset += 4;
    }
    levels
}

/// PWM value the kernel demands for a given cooling state.
pub fn cooling_level_for_state(of_dir: Option<&Path>, paths: &Paths, state: usize) -> Option<u32> {
    let level = cooling_levels(of_dir, paths).into_iter().nth(state)?;
    if level <= PWM_LIMIT {
        Some(level)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloorAssessment {
    pub floor_pwm: u8,
    pub thermal_state: u32,
    pub max_state: u32,
    pub ownership: Ownership,
    /// How the floor was derived, for the diagnostics tab.
    pub basis: String,
    pub cooling_levels: Vec<u32>,
    pub map_states: Vec<(u32, i32)>,
}

impl FloorAssessment {
    fn empty(ownership: Ownership) -> Self {
        FloorAssessment {
            floor_pwm: 0,
            thermal_state: 0,
            max_state: 0,
            ownership,
            basis: "no pwm device".to_string(),
            cooling_levels: Vec::new(),
            map_states: Vec::new(),
        }
    }
}

/// Derive the kernel safety floor, mirroring the shell `set_thermal_floor`.
pub fn evaluate_floor(paths: &Paths, fan: &FanDevice, cpu_temp: Option<i32>) -> FloorAssessment {
    let mut assessment = FloorAssessment::empty(Ownership::Unknown);
    let Some(cooling) = fan.cooling_device.as_ref() else {
        assessment.basis = "no pwm-fan cooling device; kernel floor unknown".to_string();
        return assessment;
    };
    assessment.max_state = cooling.max_state;

    let of_dir = fan.of_node();
    let dt_dir = match of_dir.as_ref() {
        Some(dir) if dir.is_dir() => Some(dir.clone()),
        _ => {
            let fallback = paths.pwm_fan_dt();
            if fallback.is_dir() {
                Some(fallback)
            } else {
                None
            }
        }
    };
    let fan_phandle = dt_dir
        .as_ref()
        .and_then(|dir| dt_be32(&dir.join("phandle"), 0));
    assessment.cooling_levels = cooling_levels(of_dir.as_deref(), paths);

    let mut maps_examined = false;
    let mut map_found = false;
    let mut best_state: u32 = 0;
    let mut matched_maps: Vec<(u32, i32)> = Vec::new();

    if let Some(fan_phandle) = fan_phandle {
        let trips = all_trips(paths);
        for map in cooling_map_dirs(paths) {
            let device = map.join("cooling-device");
            let trip_ref_path = map.join("trip");
            if !device.is_file() || !trip_ref_path.is_file() {
                continue;
            }
            let Some(map_phandle) = dt_be32(&device, 0) else {
                continue;
            };
            maps_examined = true;
            if map_phandle != fan_phandle {
                continue;
            }
            map_found = true;
            let Some(map_state) = dt_be32(&device, 1) else {
                continue;
            };
            let Some(trip_ref) = dt_be32(&trip_ref_path, 0) else {
                continue;
            };
            for (trip_path, trip) in &trips {
                let Some(trip_phandle) = dt_be32(&trip_path.join("phandle"), 0) else {
                    continue;
                };
                if trip_phandle != trip_ref {
                    continue;
                }
                let threshold = trip.temp_c.unwrap_or(0);
                let trigger_at = threshold - trip.hysteresis_c as i32;
                matched_maps.push((map_state, trigger_at));
                if let Some(current) = cpu_temp {
                    if current >= trigger_at && map_state > best_state {
                        best_state = map_state;
                    }
                }
            }
        }
    }

    assessment.map_states = matched_maps;

    if maps_examined && !map_found {
        // Exclusive mode: the fan is not in any cooling map, so only the
        // high temperature emergency states have to be enforced by us.
        assessment.ownership = Ownership::Exclusive;
        match cpu_temp {
            None => {
                assessment.thermal_state = 3;
                assessment.floor_pwm = 255;
                assessment.basis =
                    "exclusive control with no CPU reading: emergency state 3, full output"
                        .to_string();
            }
            Some(current) => {
                let mut state = 0u32;
                if current >= EMERGENCY_STATE_2_C {
                    state = 2;
                }
                if current >= EMERGENCY_STATE_3_C {
                    state = 3;
                }
                assessment.thermal_state = state;
                if state > 0 {
                    assessment.floor_pwm =
                        cooling_level_for_state(of_dir.as_deref(), paths, state as usize)
                            .map(|value| value.min(PWM_LIMIT) as u8)
                            .unwrap_or(255);
                    assessment.basis = format!(
                        "exclusive control, CPU at {current} C raised the emergency state to {state}"
                    );
                } else {
                    assessment.basis =
                        "exclusive control; no kernel cooling map demands an output".to_string();
                }
            }
        }
        return assessment;
    }

    if map_found {
        assessment.ownership = Ownership::Shared;
    }

    let Some(current) = cpu_temp else {
        // A shared map plus an unreadable CPU temperature is the one case where
        // we cannot compute the kernel demand, so assume the maximum.
        assessment.floor_pwm = 255;
        assessment.basis =
            "no CPU reading while the kernel shares fan control: full output".to_string();
        return assessment;
    };

    let mut state = best_state;
    if state == 0 {
        // Device tree discovery gave no active state; use the thresholds the
        // older H5000M firmware used so the fan still ramps up.
        if current >= FALLBACK_STATE_1_C {
            state = 1;
        }
        if current >= EMERGENCY_STATE_2_C {
            state = 2;
        }
        if current >= EMERGENCY_STATE_3_C {
            state = 3;
        }
        if state != 0 {
            assessment.basis = format!(
                "conservative fallback thresholds ({FALLBACK_STATE_1_C}/{EMERGENCY_STATE_2_C}/{EMERGENCY_STATE_3_C} C) selected state {state}"
            );
        }
    } else {
        assessment.basis = format!("kernel cooling maps selected state {state}");
    }
    assessment.thermal_state = state;
    if state == 0 {
        return assessment;
    }
    assessment.floor_pwm = cooling_level_for_state(of_dir.as_deref(), paths, state as usize)
        .map(|value| value.min(PWM_LIMIT) as u8)
        .unwrap_or(255);
    assessment
}

/// Undo the 2.0.x behaviour of switching the CPU thermal zone to `user_space`.
///
/// Returns a description when something was restored. The recorded zone has to
/// live under the configured sysfs root, so a planted file cannot make us write
/// to an arbitrary path.
pub fn restore_legacy_policy(paths: &Paths) -> Option<String> {
    let file = paths.legacy_policy_file();
    if !file.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let mut lines = text.lines();
    let zone = lines.next().unwrap_or_default().trim().to_string();
    let policy = lines.next().unwrap_or_default().trim().to_string();
    let mut note = None;
    if !zone.is_empty() && Path::new(&zone).starts_with(&paths.sysfs) {
        let policy_path = Path::new(&zone).join("policy");
        let available =
            read_trimmed(&Path::new(&zone).join("available_policies")).unwrap_or_default();
        let mut target = if available.split_whitespace().any(|name| name == policy) {
            Some(policy.clone())
        } else if available.split_whitespace().any(|name| name == "step_wise") {
            Some("step_wise".to_string())
        } else {
            None
        };
        if target.as_deref().is_some() {
            // Never hand the zone back to user_space even if that is what was
            // recorded: the whole point of the restore is to re-enable the
            // kernel governor.
            if target.as_deref() == Some("user_space") {
                target = Some("step_wise".to_string());
            }
            if let Some(value) = target.as_ref() {
                match std::fs::write(&policy_path, format!("{value}\n")) {
                    Ok(()) => {
                        note = Some(format!("restored thermal governor {value} on {zone}"));
                    }
                    Err(err) => {
                        note = Some(format!("could not restore the governor on {zone}: {err}"));
                    }
                }
            }
        } else {
            note = Some(format!("no usable governor found for {zone}"));
        }
    } else if !zone.is_empty() {
        note = Some(format!("ignored legacy policy record for {zone}"));
    }
    let _ = std::fs::remove_file(&file);
    Some(note.unwrap_or_else(|| "removed the legacy thermal policy record".to_string()))
}

/// Read the protection state without changing anything.
pub fn inspect_kernel_protection(
    paths: &Paths,
    fan_device: Option<&FanDevice>,
) -> KernelProtection {
    let zone = find_cpu_zone(paths);
    let trips = all_trips(paths).into_iter().map(|(_, trip)| trip).collect();
    let cooling = fan_device.and_then(|fan| fan.cooling_device.as_ref());
    let cpufreq_cooling = fan::cpufreq_cooling_device(paths).is_some();
    let mut protection = KernelProtection {
        zone,
        trips,
        cpufreq_cooling,
        fan_cooling_device: cooling.map(|device| device.path.display().to_string()),
        fan_max_state: cooling.map(|device| device.max_state).unwrap_or(0),
        warnings: Vec::new(),
    };
    if !cpufreq_cooling {
        protection
            .warnings
            .push("no cpufreq cooling device is registered".to_string());
    }
    match protection.zone.as_ref() {
        None => protection
            .warnings
            .push("no CPU thermal zone could be inspected".to_string()),
        Some(zone) if zone.governor_is_hijacked() => protection.warnings.push(format!(
            "thermal zone {} is using the user_space governor; CPU throttling is not being applied",
            zone.kind
        )),
        Some(_) => {
            if protection.critical_trip().is_none() && protection.hot_trip().is_none() {
                protection
                    .warnings
                    .push("no hot or critical trip is declared in the devicetree".to_string());
            }
        }
    }
    protection
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32_bytes(values: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        for value in values {
            out.extend_from_slice(&value.to_be_bytes());
        }
        out
    }

    #[test]
    fn devicetree_properties_are_read_big_endian() {
        let dir = std::env::temp_dir().join(format!("h5fan-dt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("cooling-levels");
        std::fs::write(&file, be32_bytes(&[0, 64, 128, 255])).unwrap();
        assert_eq!(dt_be32(&file, 0), Some(0));
        assert_eq!(dt_be32(&file, 1), Some(64));
        assert_eq!(dt_be32(&file, 3), Some(255));
        assert_eq!(dt_be32(&file, 4), None, "past the end is not a value");
        let truncated = dir.join("short");
        std::fs::write(&truncated, [0u8, 0, 1]).unwrap();
        assert_eq!(dt_be32(&truncated, 0), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cooling_levels_reject_values_above_the_pwm_range() {
        let dir = std::env::temp_dir().join(format!("h5fan-lvl-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("cooling-levels"), be32_bytes(&[0, 300])).unwrap();
        let paths = Paths::sandbox(dir.join("nonexistent-root"));
        assert_eq!(cooling_level_for_state(Some(&dir), &paths, 0), Some(0));
        assert_eq!(cooling_level_for_state(Some(&dir), &paths, 1), None);
        assert_eq!(cooling_level_for_state(Some(&dir), &paths, 7), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ownership_round_trips_with_the_legacy_strings() {
        assert_eq!(Ownership::Exclusive.as_str(), "userspace");
        assert_eq!(Ownership::Unknown.as_apply_str(), "userspace-pwm");
        assert_eq!(Ownership::parse("userspace-pwm"), Ownership::Exclusive);
        assert_eq!(Ownership::parse("garbage"), Ownership::Unknown);
        assert!(Ownership::Shared.describes_kernel_protection_active());
        assert!(!Ownership::Exclusive.describes_kernel_protection_active());
    }

    #[test]
    fn legacy_policy_restores_never_keep_user_space() {
        let root = std::env::temp_dir().join(format!("h5fan-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::sandbox(&root);
        let zone = paths.thermal_dir().join("thermal_zone0");
        std::fs::create_dir_all(&zone).unwrap();
        std::fs::write(zone.join("available_policies"), "step_wise fair_share\n").unwrap();
        std::fs::write(zone.join("policy"), "user_space\n").unwrap();
        std::fs::create_dir_all(&paths.run_dir).unwrap();
        std::fs::write(
            paths.legacy_policy_file(),
            format!("{}\nuser_space\n", zone.display()),
        )
        .unwrap();
        let note = restore_legacy_policy(&paths).expect("a note");
        assert!(note.contains("step_wise"), "{note}");
        assert_eq!(
            read_trimmed(&zone.join("policy")).as_deref(),
            Some("step_wise")
        );
        assert!(
            !paths.legacy_policy_file().exists(),
            "record is consumed once"
        );

        // A record pointing outside the sysfs root is dropped, not written.
        std::fs::write(paths.legacy_policy_file(), "/etc/shadow\nstep_wise\n").unwrap();
        let note = restore_legacy_policy(&paths).expect("a note");
        assert!(note.contains("ignored"), "{note}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn governor_hijack_is_detected_and_reported() {
        let root = std::env::temp_dir().join(format!("h5fan-hijack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::sandbox(&root);
        let zone = paths.thermal_dir().join("thermal_zone0");
        std::fs::create_dir_all(&zone).unwrap();
        std::fs::write(zone.join("type"), "cpu-thermal\n").unwrap();
        std::fs::write(zone.join("policy"), "user_space\n").unwrap();
        std::fs::write(zone.join("available_policies"), "step_wise user_space\n").unwrap();
        // Trip points live in the devicetree, not under the thermal zone.
        let trip = paths
            .cooling_map_root()
            .join("cpu-thermal")
            .join("trips")
            .join("trip-point2");
        std::fs::create_dir_all(&trip).unwrap();
        std::fs::write(trip.join("type"), "critical\n").unwrap();
        std::fs::write(trip.join("temperature"), be32_bytes(&[125000])).unwrap();
        let protection = inspect_kernel_protection(&paths, None);
        assert!(protection.zone.as_ref().unwrap().governor_is_hijacked());
        assert!(
            protection.critical_trip().is_some(),
            "the critical trip must be found in the devicetree"
        );
        assert!(
            !protection.protections_intact(),
            "a user_space governor must be reported as broken protection"
        );
        assert!(protection
            .warnings
            .iter()
            .any(|warning| warning.contains("user_space")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
