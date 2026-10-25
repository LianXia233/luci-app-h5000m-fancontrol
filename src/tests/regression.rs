//! End-to-end regression tests against a simulated H5000M environment.
//!
//! These drive the real control cycle, the real status report and the real
//! daemon loop, with every hardware path pointed at a temporary directory. No
//! test writes to a live PWM node, stops a live fan or touches a thermal zone.

mod common;

use common::{mc, Fixture, StubProbe};
use h5000m_fancontrol::cli::{self, Command, Format};
use h5000m_fancontrol::controller;
use h5000m_fancontrol::daemon::{self, ApplyOutcome};
use h5000m_fancontrol::lock::{Acquisition, Lock};
use h5000m_fancontrol::paths::Paths;
use h5000m_fancontrol::platform::{self, Signals};
use h5000m_fancontrol::report::{self, Health, SampleMode};
use h5000m_fancontrol::safety::{self, Ownership};
use h5000m_fancontrol::sensor::SensorId;
use h5000m_fancontrol::state::{ControlResult, Snapshot};
use std::time::Duration;

/// A board that looks like a booted H5000M: CPU zone, PHY, two radios, a fan,
/// both cooling devices and the device tree the shipped firmware has.
fn prepared(label: &str) -> Fixture {
    let fixture = Fixture::new(label);
    fixture.thermal_zone("cpu-thermal", mc(52));
    fixture.hwmon("mt7981_thermal", Some(mc(54)));
    fixture.hwmon("Phy-0", Some(mc(48)));
    fixture.hwmon("mt7996_wiphy", Some(mc(57)));
    fixture.hwmon("mt799a_wiphy", Some(mc(61)));
    fixture.pwm_fan(0, Some(4800));
    fixture.pwm_fan_cooling_device(3);
    fixture.cpufreq_cooling_device();
    // The firmware declares 8 bit cooling levels and the usual trip ladder.
    fixture.dt_pwm_fan(7, &[0, 64, 128, 255]);
    fixture.dt_trip("cpu-thermal", "trip-point0", "passive", 95_000, 2_000, 11);
    fixture.dt_trip("cpu-thermal", "trip-point1", "hot", 105_000, 2_000, 12);
    fixture.dt_trip("cpu-thermal", "trip-point2", "critical", 115_000, 0, 13);
    fixture.uci_defaults();
    fixture
}

fn cycle(fixture: &Fixture, probe: &StubProbe, previous: Option<&Snapshot>, now: u64) -> Snapshot {
    controller::run_cycle(&fixture.paths, probe, previous, now, &mut |_| {})
}

#[test]
fn native_sensors_win_and_the_vendor_helper_is_not_called() {
    let fixture = prepared("native-priority");
    let probe = StubProbe::new().answering("ra0", "CurrentTemperature = 30");
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(sensors.value(SensorId::Wifi1), Some(57));
    assert_eq!(sensors.value(SensorId::Wifi2), Some(61));
    assert_eq!(
        probe.call_count(),
        0,
        "a native reading must not query iwpriv"
    );
    assert_eq!(sensors.value(SensorId::Cpu), Some(52));
    assert_eq!(sensors.value(SensorId::Phy), Some(48));
}

#[test]
fn vendor_fallback_fills_only_the_missing_radio_slots() {
    let fixture = Fixture::new("vendor-fallback");
    fixture.thermal_zone("cpu-thermal", mc(50));
    fixture.pwm_fan(0, None);
    fixture.uci_defaults();
    fixture.radio_interface("ra0");
    fixture.radio_interface("rai0");
    // Only one radio has a native sensor.
    fixture.hwmon("mt7996_wiphy", Some(mc(60)));
    let probe = StubProbe::new()
        .answering("ra0", "CurrentTemperature = 41")
        .answering("rai0", "CurrentTemperature = 66");
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(sensors.value(SensorId::Wifi1), Some(60));
    assert_eq!(
        sensors.value(SensorId::Wifi2),
        Some(66),
        "the empty slot is filled from the driver"
    );
    assert_eq!(
        sensors
            .get(SensorId::Wifi2)
            .and_then(|reading| reading.source.as_deref()),
        Some("iwpriv:rai0")
    );
    assert_eq!(probe.call_count(), 1, "only the missing slot is queried");
}

#[test]
fn vendor_helper_timeout_and_failures_leave_the_temperature_unknown() {
    let fixture = Fixture::new("vendor-timeout");
    fixture.thermal_zone("cpu-thermal", mc(50));
    fixture.pwm_fan(0, None);
    fixture.uci_defaults();
    fixture.radio_interface("ra0");
    fixture.radio_interface("rai0");
    let probe = StubProbe::new().timing_out("ra0").failing("rai0");
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    for id in [SensorId::Wifi1, SensorId::Wifi2] {
        let reading = sensors.get(id).unwrap();
        assert_eq!(reading.value_c, None, "{id:?} must stay unknown");
        assert_eq!(
            reading.state,
            h5000m_fancontrol::sensor::SensorState::Failed
        );
        assert!(reading.error.is_some());
    }
    let absent = StubProbe::without_helper(StubProbe::new());
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &absent, 1000);
    assert!(sensors
        .get(SensorId::Wifi1)
        .unwrap()
        .error
        .as_deref()
        .unwrap()
        .contains("iwpriv is not installed"));
}

#[test]
fn module_cache_respects_freshness_and_rejects_future_timestamps() {
    let fixture = prepared("module-cache");
    let now = platform::wall_now();
    fixture.modem_cache("43.6", now);
    let probe = StubProbe::new();
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, now);
    assert_eq!(
        sensors.value(SensorId::Module),
        Some(44),
        "43.6 rounds to 44"
    );

    let stale_now = now + 120;
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, stale_now);
    assert_eq!(sensors.value(SensorId::Module), None, "stale is unknown");
    assert_eq!(
        sensors.get(SensorId::Module).unwrap().state,
        h5000m_fancontrol::sensor::SensorState::Stale
    );

    let future = now + 3600;
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, future);
    assert_eq!(sensors.value(SensorId::Module), None);
    assert_eq!(
        sensors.get(SensorId::Module).unwrap().state,
        h5000m_fancontrol::sensor::SensorState::Stale
    );
}

#[test]
fn legacy_modem_cache_is_scanned_and_range_checked() {
    let fixture = prepared("legacy-cache");
    common::write(
        &fixture.paths.legacy_cache_dir.join("cache_base_info_1"),
        "{\"data\":[{\"key\":\"temperature\",\"unit\":\"C\",\"value\":\"-70\"},{\"key\":\"rssi\",\"value\":\"-40\"}]}\n",
    );
    let probe = StubProbe::new();
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(
        sensors.value(SensorId::Module),
        None,
        "-70 C is out of range and must not become a reading"
    );
    common::write(
        &fixture.paths.legacy_cache_dir.join("cache_base_info_1"),
        "{\"data\":[{\"key\":\"temperature\",\"value\":\"38.9\"}]}\n",
    );
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(
        sensors.value(SensorId::Module),
        Some(38),
        "the legacy member is read as an integer, like grep -Eo did"
    );
    assert_eq!(
        sensors.get(SensorId::Module).unwrap().source.as_deref(),
        Some("legacy-cache")
    );
}

#[test]
fn out_of_range_and_malformed_temperatures_are_never_zero() {
    let fixture = Fixture::new("bad-temps");
    let zone = fixture.paths.thermal_dir().join("thermal_zone0");
    common::write(&zone.join("type"), "cpu-thermal\n");
    common::write(&zone.join("temp"), "250000\n");
    common::write(&zone.join("policy"), "step_wise\n");
    common::write(&zone.join("available_policies"), "step_wise\n");
    fixture.pwm_fan(0, None);
    fixture.uci_defaults();
    let probe = StubProbe::new();
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(sensors.value(SensorId::Cpu), None);
    assert_eq!(
        sensors.get(SensorId::Cpu).unwrap().state,
        h5000m_fancontrol::sensor::SensorState::Invalid
    );

    // Millidegrees are converted, plain degrees are kept.
    common::write(&zone.join("temp"), "61500\n");
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(sensors.value(SensorId::Cpu), Some(61));
    common::write(&zone.join("temp"), "47\n");
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(sensors.value(SensorId::Cpu), Some(47));
}

#[test]
fn control_temperature_follows_the_configured_source() {
    let fixture = prepared("temp-source");
    let settings = h5000m_fancontrol::config::Settings::load(&fixture.paths).unwrap();
    let probe = StubProbe::new();
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    assert_eq!(
        sensors.control(&settings).map(|(_, value)| value),
        Some(61),
        "max mode picks the hottest radio"
    );
    let mut cpu_only = settings.clone();
    cpu_only.temp_source = h5000m_fancontrol::config::TempSource::Cpu;
    assert_eq!(sensors.control(&cpu_only).map(|(_, value)| value), Some(52));
}

#[test]
fn automatic_cycle_writes_the_curve_output_and_publishes_state() {
    let fixture = prepared("auto-cycle");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.result, ControlResult::Ok);
    assert_eq!(snapshot.reason, "auto");
    // 61 C on the balanced curve: 60 + (61-55)*(75-60)/(70-55) = 66 percent,
    // and 66 * 255 / 100 truncates to 168.
    assert_eq!(snapshot.target_pwm, 168);
    assert_eq!(
        fixture.read_pwm(&fan_dir),
        Some(168),
        "the register must hold what was decided"
    );
    assert_eq!(snapshot.control_temp, Some(61));
    assert_eq!(snapshot.applied_pwm, 168);
    assert_eq!(snapshot.rpm, Some(4800));
    assert!(snapshot.pid > 0);
    assert_eq!(snapshot.cycle_count, 1);
    // A cycle computes and applies; publishing is the daemon's job.
    fixture.publish_state(&snapshot.encode());
    let published = fixture.state_text().expect("a state file was written");
    assert!(published.contains("applied_pwm=168"), "{published}");
    assert!(published.contains("cpu_temp=52"), "{published}");
    assert!(published.contains("fan_rpm") || !published.contains("fan_rpm="));
}

#[test]
fn repeated_cycle_at_the_same_temperature_does_not_rewrite_the_register() {
    let fixture = prepared("no-churn");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    let probe = StubProbe::new();
    let first = cycle(&fixture, &probe, None, 1000);
    // Let the file timestamp granularity separate from a real rewrite.
    std::thread::sleep(Duration::from_millis(30));
    let before = std::fs::metadata(fan_dir.join("pwm1"))
        .unwrap()
        .modified()
        .unwrap();
    let second = controller::run_cycle(&fixture.paths, &probe, Some(&first), 1006, &mut |_| {});
    let after = std::fs::metadata(fan_dir.join("pwm1"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(second.target_pwm, first.target_pwm);
    assert_eq!(before, after, "an unchanged target must not be rewritten");
}

#[test]
fn manual_mode_writes_the_configured_output_and_kernel_mode_does_not_write() {
    let fixture = prepared("manual-kernel");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    let probe = StubProbe::new();
    fixture.uci("config settings 'settings'\n\toption mode 'manual'\n\toption manual_pwm '100'\n");
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.reason, "manual");
    assert_eq!(fixture.read_pwm(&fan_dir), Some(100));

    fixture.uci("config settings 'settings'\n\toption mode 'kernel'\n");
    common::write(&fan_dir.join("pwm1"), "77\n");
    let snapshot =
        controller::run_cycle(&fixture.paths, &probe, Some(&snapshot), 1005, &mut |_| {});
    assert_eq!(snapshot.reason, "kernel");
    assert_eq!(snapshot.thermal_owner, "kernel");
    assert_eq!(
        fixture.read_pwm(&fan_dir),
        Some(77),
        "kernel-only mode must leave the register to the kernel"
    );
    assert_eq!(snapshot.target_pwm, 77);

    fixture.uci("config settings 'settings'\n\toption enabled '0'\n");
    let snapshot =
        controller::run_cycle(&fixture.paths, &probe, Some(&snapshot), 1010, &mut |_| {});
    assert_eq!(snapshot.reason, "kernel");
    assert_eq!(fixture.read_pwm(&fan_dir), Some(77));
}

#[test]
fn losing_every_sensor_forces_the_high_output_failsafe() {
    let fixture = Fixture::new("failsafe");
    fixture.pwm_fan(20, None);
    fixture.uci_defaults();
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.result, ControlResult::Failsafe);
    assert_eq!(snapshot.reason, "sensor-failsafe");
    assert_eq!(snapshot.requested_pwm, 255);
    assert_eq!(
        fixture.read_pwm(&fixture.paths.hwmon_dir().join("hwmon1")),
        Some(255),
        "failsafe must override the 20 the fan sat at"
    );
    assert!(snapshot.control_temp.is_none());
}

#[test]
fn an_invalid_curve_fails_safe_instead_of_stopping_the_fan() {
    let fixture = prepared("curve-failsafe");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.uci(
        "config settings 'settings'\n\toption curve 'custom'\n\toption curve_custom '50:100,40:80'\n",
    );
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.reason, "curve-failsafe");
    assert_eq!(snapshot.result, ControlResult::Failsafe);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(255));
}

#[test]
fn kernel_cooling_map_sets_a_floor_the_controller_may_not_undercut() {
    let fixture = prepared("shared-floor");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    // pwm-fan phandle 7, levels 0/64/128/255, an active map at 45 C to state 2.
    fixture.dt_pwm_fan(7, &[0, 64, 128, 255]);
    fixture.dt_trip("cpu-thermal", "trip-point0", "active", 45_000, 3_000, 21);
    fixture.dt_cooling_map("cpu-thermal", "cpu-active-low", 7, 2, 21);
    fixture.uci("config settings 'settings'\n\toption mode 'manual'\n\toption manual_pwm '10'\n");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.thermal_owner, "shared");
    assert_eq!(snapshot.thermal_floor, 128, "cooling level of state 2");
    assert_eq!(snapshot.target_pwm, 128, "the manual request is raised");
    assert!(snapshot.kernel_floor_applied);
    assert_eq!(snapshot.reason, "manual+kernel-floor");
    assert_eq!(fixture.read_pwm(&fan_dir), Some(128));
    assert!(
        snapshot.thermal_basis.contains("cooling maps"),
        "{}",
        snapshot.thermal_basis
    );
}

#[test]
fn exclusive_control_still_enforces_the_emergency_states() {
    let fixture = prepared("exclusive-floor");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    // Maps exist but reference another device (phandle 9), so the fan is ours.
    fixture.dt_pwm_fan(7, &[0, 64, 128, 255]);
    fixture.dt_trip("cpu-thermal", "trip-point0", "active", 45_000, 0, 21);
    fixture.dt_cooling_map("cpu-thermal", "other-active", 9, 1, 21);
    fixture.uci("config settings 'settings'\n\toption mode 'manual'\n\toption manual_pwm '20'\n");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.thermal_owner, "userspace");
    assert_eq!(
        snapshot.thermal_floor, 0,
        "52 C is below the emergency point"
    );
    assert_eq!(fixture.read_pwm(&fan_dir), Some(20));

    // Push the CPU past the 83 C emergency state and run again.
    common::write(
        &fixture.paths.thermal_dir().join("thermal_zone0/temp"),
        format!("{}\n", mc(85)),
    );
    let snapshot =
        controller::run_cycle(&fixture.paths, &probe, Some(&snapshot), 1005, &mut |_| {});
    assert_eq!(snapshot.thermal_state, 2);
    assert_eq!(snapshot.thermal_floor, 128);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(128));

    common::write(
        &fixture.paths.thermal_dir().join("thermal_zone0/temp"),
        format!("{}\n", mc(120)),
    );
    let snapshot =
        controller::run_cycle(&fixture.paths, &probe, Some(&snapshot), 1010, &mut |_| {});
    assert_eq!(snapshot.thermal_state, 3);
    assert_eq!(snapshot.thermal_floor, 255);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(255));
}

#[test]
fn missing_cpu_reading_in_shared_mode_demands_full_output() {
    let fixture = prepared("shared-no-cpu");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.dt_pwm_fan(7, &[0, 64, 128, 255]);
    fixture.dt_trip("cpu-thermal", "trip-point0", "active", 45_000, 0, 21);
    fixture.dt_cooling_map("cpu-thermal", "cpu-active-low", 7, 1, 21);
    // Remove the CPU sensor entirely.
    std::fs::remove_file(fixture.paths.thermal_dir().join("thermal_zone0/temp")).unwrap();
    std::fs::remove_dir_all(fixture.paths.hwmon_dir().join("hwmon1")).ok();
    fixture.uci("config settings 'settings'\n\toption mode 'manual'\n\toption manual_pwm '15'\n");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.thermal_floor, 255);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(255));
    assert_eq!(snapshot.result, ControlResult::Ok, "manual still applies");
}

#[test]
fn spin_down_is_held_then_released_after_the_delay() {
    let fixture = prepared("down-delay");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.uci("config settings 'settings'\n\toption down_delay '60'\n\toption hysteresis '2'\n");
    let probe = StubProbe::new();
    // Warm first: the hottest sensor is the 61 C radio.
    let hot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(hot.target_pwm, 168);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(168));

    // Cool everything down to 38 C, where the curve asks for far less.
    common::write(
        &fixture.paths.thermal_dir().join("thermal_zone0/temp"),
        format!("{}\n", mc(38)),
    );
    for index in 1..=4 {
        let file = fixture
            .paths
            .hwmon_dir()
            .join(format!("hwmon{index}"))
            .join("temp1_input");
        if file.exists() {
            common::write(&file, format!("{}\n", mc(38)));
        }
    }
    let held = controller::run_cycle(&fixture.paths, &probe, Some(&hot), 1010, &mut |_| {});
    assert_eq!(held.control_temp, Some(38));
    assert_eq!(held.reason, "auto-down-delay");
    assert_eq!(held.target_pwm, 168, "the fan must not slow down yet");
    assert_eq!(held.down_since, 1010);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(168));

    let released = controller::run_cycle(&fixture.paths, &probe, Some(&held), 1080, &mut |_| {});
    assert_eq!(released.reason, "auto");
    assert!(
        released.target_pwm < 168,
        "the delay expired, so the curve decides: {}",
        released.target_pwm
    );
    assert_eq!(released.down_since, 0);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(released.target_pwm));
}

#[test]
fn a_stale_snapshot_does_not_hold_the_fan_at_an_old_speed() {
    let fixture = prepared("stale-state");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    let probe = StubProbe::new();
    // Published long ago, claiming the fan was at full speed.
    let mut ancient = Snapshot::published_now(1_000);
    ancient.result = ControlResult::Ok;
    ancient.reason = "auto".to_string();
    ancient.applied_pwm = 255;
    common::write(&fan_dir.join("pwm1"), "60\n");
    let fresh = controller::run_cycle(&fixture.paths, &probe, Some(&ancient), 10_000, &mut |_| {});
    assert_eq!(
        fresh.target_pwm, 168,
        "a 9000 s old applied_pwm must not force full speed"
    );
    assert_eq!(fixture.read_pwm(&fan_dir), Some(168));
}

#[test]
fn startup_boost_runs_before_the_target_is_applied() {
    let fixture = prepared("boost");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.uci(
        "config settings 'settings'\n\toption start_pwm '220'\n\toption start_boost_ms '700'\n",
    );
    let probe = StubProbe::new();
    let mut during = Vec::new();
    let mut pauses = Vec::new();
    let snapshot = controller::run_cycle(&fixture.paths, &probe, None, 1000, &mut |duration| {
        pauses.push(duration);
        during.push(
            std::fs::read_to_string(fan_dir.join("pwm1"))
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
        );
    });
    assert!(snapshot.boost_applied);
    assert_eq!(
        pauses,
        vec![Duration::from_millis(700)],
        "exact boost length"
    );
    assert_eq!(during, vec!["220".to_string()], "boost level first");
    assert_eq!(fixture.read_pwm(&fan_dir).unwrap(), snapshot.target_pwm);
}

#[test]
fn a_boost_can_be_disabled_and_still_starts_the_fan_to_target() {
    let fixture = prepared("no-boost");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.uci("config settings 'settings'\n\toption start_boost_ms '0'\n");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert!(!snapshot.boost_applied);
    assert_eq!(fixture.read_pwm(&fan_dir), Some(snapshot.target_pwm));
}

#[test]
fn a_pwm_node_that_rejects_writes_is_reported_as_a_failed_cycle() {
    let fixture = prepared("write-fail");
    // Replace the writable register with a directory: reads and writes both fail.
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    std::fs::remove_file(fan_dir.join("pwm1")).unwrap();
    std::fs::create_dir(fan_dir.join("pwm1")).unwrap();
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.result, ControlResult::Error);
    assert_eq!(snapshot.reason, "pwm-write-failed");
    assert!(snapshot.last_error.is_some());
    assert_eq!(snapshot.consecutive_failures, 1);
    assert_eq!(snapshot.cycle_count, 1);
}

#[test]
fn a_missing_pwm_node_publishes_an_explanatory_error() {
    let fixture = Fixture::new("no-pwm");
    fixture.thermal_zone("cpu-thermal", mc(60));
    fixture.uci_defaults();
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.result, ControlResult::Error);
    assert_eq!(snapshot.reason, "pwm-missing");
    assert!(
        fixture.state_text().is_none(),
        "run_cycle does not write; apply does"
    );
    match daemon::apply(&fixture.paths) {
        Ok(ApplyOutcome::Applied(snapshot)) => {
            assert_eq!(snapshot.reason, "pwm-missing");
        }
        other => panic!("expected a failed cycle, got {other:?}"),
    }
    assert_eq!(
        cli::execute(Command::Apply, &fixture.paths),
        1,
        "apply keeps the 2.x exit code for a failed cycle"
    );
}

#[test]
fn the_controller_never_changes_the_kernel_governor() {
    let fixture = prepared("governor");
    let zone = fixture.paths.thermal_dir().join("thermal_zone0");
    let before = fixture.read_policy(&zone);
    let probe = StubProbe::new();
    let mut previous = None;
    for second in 1000..1030 {
        let snapshot = controller::run_cycle(
            &fixture.paths,
            &probe,
            previous.as_ref(),
            second,
            &mut |_| {},
        );
        previous = Some(snapshot);
    }
    assert_eq!(
        fixture.read_policy(&zone).as_deref(),
        before.as_deref(),
        "the thermal governor must be untouched"
    );
    assert!(
        !fixture.paths.legacy_policy_file().exists(),
        "the controller must not record a takeover"
    );
    let protection = safety::inspect_kernel_protection(&fixture.paths, None);
    assert!(protection.cpufreq_cooling, "throttling stays registered");
    assert!(!protection.zone.as_ref().unwrap().governor_is_hijacked());
}

#[test]
fn automatic_control_never_undercuts_the_kernel_floor_end_to_end() {
    // The most safety relevant rule in the whole controller: when a kernel
    // cooling map demands more airflow than the curve does, the map wins.
    let fixture = prepared("auto-floor");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.dt_pwm_fan(7, &[0, 64, 128, 255]);
    fixture.dt_trip("cpu-thermal", "trip-point3", "active", 50_000, 0, 31);
    fixture.dt_cooling_map("cpu-thermal", "cpu-active-high", 7, 3, 31);
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.thermal_owner, "shared");
    assert_eq!(snapshot.thermal_floor, 255, "cooling level of state 3");
    assert_eq!(
        snapshot.requested_pwm, 168,
        "the curve itself still asked for 66 percent"
    );
    assert_eq!(snapshot.target_pwm, 255, "the kernel floor must win");
    assert!(snapshot.kernel_floor_applied);
    assert_eq!(snapshot.reason, "auto+kernel-floor");
    assert_eq!(
        fixture.read_pwm(&fan_dir),
        Some(255),
        "the register must never sit below the kernel demand"
    );
}

#[test]
fn a_legacy_user_space_takeover_is_repaired_once() {
    let fixture = prepared("legacy-restore");
    let zone = fixture.paths.thermal_dir().join("thermal_zone0");
    common::write(&zone.join("policy"), "user_space\n");
    common::write(
        &fixture.paths.legacy_policy_file(),
        format!("{}\nstep_wise\n", zone.display()),
    );
    let probe = StubProbe::new();
    match daemon::apply(&fixture.paths) {
        Ok(ApplyOutcome::Applied(_)) => {}
        other => panic!("apply should run, got {other:?}"),
    }
    assert_eq!(
        fixture.read_policy(&zone).as_deref(),
        Some("step_wise"),
        "the recorded governor is restored"
    );
    assert!(
        !fixture.paths.legacy_policy_file().exists(),
        "the record is consumed"
    );
    // A second apply must not touch it again.
    let _ = daemon::apply(&fixture.paths);
    assert_eq!(fixture.read_policy(&zone).as_deref(), Some("step_wise"));
    let protection = safety::inspect_kernel_protection(&fixture.paths, None);
    assert!(protection.protections_intact());
    let _ = probe;
}

#[test]
fn broken_kernel_protection_is_surfaced_in_the_status() {
    let fixture = prepared("broken-protection");
    let zone = fixture.paths.thermal_dir().join("thermal_zone0");
    common::write(&zone.join("policy"), "user_space\n");
    let probe = StubProbe::new();
    let report = report::build(&fixture.paths, &probe, SampleMode::Live, 1000).unwrap();
    assert!(
        !report.protection.protections_intact(),
        "a hijacked governor must be reported as broken"
    );
    let json = report.to_value(&fixture.paths).to_json();
    assert!(json.contains("\"governor_hijacked\":true"), "{json}");
    assert!(json.contains("\"intact\":false"));
}

#[test]
fn apply_defers_to_a_live_daemon_instead_of_racing_it() {
    let fixture = prepared("defer");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    fixture.touch_own_proc();
    let lock = match Lock::try_acquire(&fixture.paths) {
        Acquisition::Acquired(lock) => lock,
        other => panic!("the fixture must be able to take the lock: {other:?}"),
    };
    common::write(&fan_dir.join("pwm1"), "33\n");
    match daemon::apply(&fixture.paths) {
        Ok(ApplyOutcome::Queued { pid }) => assert!(pid > 0),
        other => panic!("a live daemon must be deferred to, got {other:?}"),
    }
    assert_eq!(
        fixture.read_pwm(&fan_dir),
        Some(33),
        "the deferred apply must not write the register"
    );
    assert!(fixture.paths.wake_file().exists(), "the daemon was nudged");
    drop(lock);
    assert!(!fixture.paths.lock_dir.exists());
}

#[test]
fn a_dead_owner_lets_the_next_apply_take_over() {
    let fixture = prepared("dead-owner");
    // A lock file pointing at a pid that is not this process and has no proc
    // entry in the sandbox: it must be reclaimed, not honoured.
    std::fs::create_dir_all(&fixture.paths.lock_dir).unwrap();
    common::write(&fixture.paths.lock_dir.join("pid"), "4242424\n");
    match daemon::apply(&fixture.paths) {
        Ok(ApplyOutcome::Applied(_)) => {}
        other => panic!("the stale lock must be reclaimed, got {other:?}"),
    }
    assert!(
        !fixture.paths.lock_dir.exists(),
        "apply releases the lock again"
    );
}

#[test]
fn the_daemon_loop_publishes_cycles_and_stops_cleanly() {
    let fixture = prepared("daemon-loop");
    let paths = fixture.paths.clone();
    let signals = Signals::new();
    let started = signals.clone();
    let handle = std::thread::spawn(move || daemon::run(&paths, &started));
    // Wait for at least one published cycle.
    let mut published = None;
    for _ in 0..60 {
        if let Ok(text) = std::fs::read_to_string(fixture.paths.state_file()) {
            published = Some(Snapshot::decode(&text));
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let snapshot = published.expect("the daemon publishes a snapshot");
    assert_eq!(snapshot.result, ControlResult::Ok);
    assert!(snapshot.cycle_count >= 1);
    assert!(snapshot.pid > 0);
    assert!(
        fixture.paths.event_log().exists(),
        "the daemon keeps an event journal"
    );
    signals.request_stop();
    let result = handle.join().expect("the daemon exits");
    assert_ne!(result, ControlResult::Error);
    assert!(
        !fixture.paths.lock_dir.exists(),
        "the lock is released on shutdown"
    );
}

#[test]
fn a_config_edit_is_applied_without_restarting_the_controller() {
    let fixture = prepared("hot-edit");
    let fan_dir = fixture.paths.hwmon_dir().join("hwmon5");
    let probe = StubProbe::new();
    let first = cycle(&fixture, &probe, None, 1000);
    assert_eq!(first.reason, "auto");
    fixture.uci("config settings 'settings'\n\toption mode 'manual'\n\toption manual_pwm '12'\n");
    let second = controller::run_cycle(&fixture.paths, &probe, Some(&first), 1005, &mut |_| {});
    assert!(
        second.reason.starts_with("manual"),
        "the edit must take effect in the same process: {}",
        second.reason
    );
    assert_eq!(
        second.target_pwm, second.thermal_floor,
        "12 is below the kernel floor, so the floor wins"
    );
    assert_eq!(fixture.read_pwm(&fan_dir), Some(second.target_pwm));
    assert_eq!(second.cycle_count, 2, "the same process kept counting");
}

#[test]
fn cached_status_reuses_the_snapshot_and_live_status_resamples() {
    let fixture = Fixture::new("status-modes");
    fixture.thermal_zone("cpu-thermal", mc(52));
    fixture.pwm_fan(0, None);
    fixture.uci_defaults();
    fixture.radio_interface("ra0");
    fixture.radio_interface("rai0");
    // No radio has a native sensor, so every live sample touches the helper.
    let probe = StubProbe::new()
        .answering("ra0", "CurrentTemperature = 44")
        .answering("rai0", "CurrentTemperature = 51");

    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(probe.call_count(), 2, "one query per missing radio");
    fixture.publish_state(&snapshot.encode());

    let before = probe.call_count();
    let cached = report::build(&fixture.paths, &probe, SampleMode::Cached, 1002).unwrap();
    assert_eq!(cached.mode, SampleMode::Cached);
    assert_eq!(
        cached.control_temp,
        Some(52),
        "the cached control temperature comes from the snapshot"
    );
    assert_eq!(cached.sensors.len(), 5);
    assert_eq!(
        probe.call_count(),
        before,
        "a browser poll must not probe the radio driver"
    );

    let live = report::build(&fixture.paths, &probe, SampleMode::Live, 1004).unwrap();
    assert_eq!(probe.call_count(), before + 2, "live mode resamples");
    assert_eq!(live.sensors.len(), 5);
    assert_eq!(live.health(), Health::Running);
    assert_eq!(live.reason, "auto");
}

#[test]
fn status_reports_unknowns_as_empty_or_null_in_both_formats() {
    let fixture = Fixture::new("status-unknown");
    fixture.pwm_fan(10, None);
    fixture.uci_defaults();
    let probe = StubProbe::new();
    let report = report::build(&fixture.paths, &probe, SampleMode::Live, 1000).unwrap();
    let plain = report.to_plain();
    for key in [
        "cpu_temp=\n",
        "phy_temp=\n",
        "wifi1_temp=\n",
        "wifi2_temp=\n",
        "module_temp=\n",
        "control_temp=\n",
        "fan_rpm=\n",
    ] {
        assert!(plain.contains(key), "expected {key} to be empty:\n{plain}");
    }
    assert!(plain.contains("cpu_state=missing"), "{plain}");
    let json = report.to_value(&fixture.paths).to_json();
    assert!(json.contains(r#""value_c":null"#), "{json}");
    assert!(
        !json.contains(r#""value_c":0,="#),
        "a missing temperature must never serialise as zero"
    );
    assert_eq!(report.health(), Health::Stopped, "nothing was published");
    assert!(!report.published);
}

#[test]
fn every_legacy_status_key_is_still_present() {
    let fixture = prepared("legacy-keys");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    fixture.publish_state(&snapshot.encode());
    let report = report::build(&fixture.paths, &probe, SampleMode::Live, 1002).unwrap();
    let plain = report.to_plain();
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
    // Values the 2.x controller produced for the same hardware still match.
    assert!(plain.contains("mode=auto"));
    assert!(plain.contains("fan_feedback=1"));
    assert!(plain.contains("result=ok"));
}

#[test]
fn sensor_provenance_and_age_are_published_per_sensor() {
    let fixture = prepared("provenance");
    let probe = StubProbe::new();
    let snapshot = cycle(&fixture, &probe, None, 1000);
    fixture.publish_state(&snapshot.encode());
    let report = report::build(&fixture.paths, &probe, SampleMode::Cached, 1010).unwrap();
    let json = report.to_value(&fixture.paths);
    let value = json.to_json();
    assert!(
        value.contains("\"source\":\"hwmon:mt7996_wiphy\""),
        "{value}"
    );
    assert!(value.contains("\"status\":\"missing\""));
    let cpu = report
        .sensors
        .iter()
        .find(|reading| reading.id == SensorId::Cpu)
        .unwrap();
    assert_eq!(cpu.source.as_deref(), Some("thermal-zone:cpu-thermal"));
    assert_eq!(cpu.label.as_deref(), Some("cpu-thermal"));
    assert_eq!(cpu.state, h5000m_fancontrol::sensor::SensorState::Ok);
    let module = report
        .sensors
        .iter()
        .find(|reading| reading.id == SensorId::Module)
        .unwrap();
    assert_eq!(
        module.state,
        h5000m_fancontrol::sensor::SensorState::Missing
    );
    assert!(module.error.is_some(), "the reason is explained");
}

#[test]
fn contract_lists_every_field_the_frontend_reads() {
    let contract = report::contract();
    for path in [
        "controller.health",
        "controller.state_age_s",
        "controller.last_error",
        "settings.mode",
        "settings.curve_data",
        "settings.curve_valid",
        "settings.issues",
        "fan.pwm_value",
        "fan.requested_pwm",
        "fan.target_pwm",
        "fan.applied_pwm",
        "fan.rpm",
        "fan.reason",
        "fan.down_delay_remaining_s",
        "thermal.owner",
        "thermal.floor_pwm",
        "thermal.protections.intact",
        "thermal.protections.trips",
        "control.temp_c",
        "sensors[].value_c",
        "sensors[].status",
        "sensors[].source",
        "sensors[].age_s",
        "events[].code",
        "sample_mode",
    ] {
        assert!(
            contract.iter().any(|found| found == path),
            "the contract is missing {path}"
        );
    }
}

#[test]
fn the_daemon_journal_records_the_failsafe_transition_and_stays_bounded() {
    // A board with a fan but no readable sensor: the loop goes into failsafe on
    // its very first cycle, which is exactly what must show up in the journal.
    let fixture = Fixture::new("journal");
    fixture.pwm_fan(0, None);
    fixture.uci("config settings 'settings'\n\toption interval '2'\n");
    let paths = fixture.paths.clone();
    let signals = Signals::new();
    let handle = {
        let signals = signals.clone();
        std::thread::spawn(move || daemon::run(&paths, &signals))
    };
    let mut journal = Vec::new();
    for _ in 0..150 {
        let entries = h5000m_fancontrol::events::read_recent(&fixture.paths, 50);
        if entries
            .iter()
            .any(|event| event.code == "control" && event.message.contains("sensor-failsafe"))
        {
            journal = entries;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    signals.request_stop();
    let _ = handle.join();
    assert!(
        journal
            .iter()
            .any(|event| event.code == "control" && event.message.contains("sensor-failsafe")),
        "the failsafe entry is missing from {journal:?}"
    );
    assert!(
        journal.iter().any(|event| event.code == "start"),
        "the daemon logs its start"
    );
    let raw = std::fs::read_to_string(fixture.paths.event_log()).unwrap();
    assert!(
        raw.len() < 64 * 1024,
        "the journal is bounded ({} bytes)",
        raw.len()
    );
}

#[test]
fn validate_command_reports_an_unusable_curve_and_the_cli_exit_code() {
    let fixture = prepared("validate");
    assert_eq!(cli::execute(Command::Validate, &fixture.paths), 0);
    fixture
        .uci("config settings 'settings'\n\toption curve 'custom'\n\toption curve_custom 'oops'\n");
    assert_eq!(
        cli::execute(Command::Validate, &fixture.paths),
        1,
        "an unusable curve must fail validation"
    );
}

#[test]
fn status_json_command_produces_a_parseable_document() {
    let fixture = prepared("status-command");
    let code = cli::execute(
        Command::Status {
            mode: SampleMode::Cached,
            format: Format::Json,
        },
        &fixture.paths,
    );
    assert_eq!(code, 0);
    let code = cli::execute(
        Command::Status {
            mode: SampleMode::Live,
            format: Format::Plain,
        },
        &fixture.paths,
    );
    assert_eq!(code, 0);
    assert_eq!(cli::execute(Command::Schema, &fixture.paths), 0);
    assert_eq!(
        cli::execute(Command::Events { limit: 5 }, &fixture.paths),
        0
    );
}

#[test]
fn ownership_strings_stay_compatible_with_the_2_x_consumers() {
    let fixture = prepared("ownership");
    let device = h5000m_fancontrol::fan::discover(&fixture.paths).unwrap();
    let probe = StubProbe::new();
    let sensors = h5000m_fancontrol::sensor::sample_all(&fixture.paths, &probe, 1000);
    let assessment = safety::evaluate_floor(&fixture.paths, &device, sensors.value(SensorId::Cpu));
    // No devicetree at all in this fixture: unknown, which the controller
    // publishes as the 2.x `userspace-pwm` string.
    assert_eq!(assessment.ownership, Ownership::Unknown);
    assert_eq!(assessment.ownership.as_apply_str(), "userspace-pwm");
    let snapshot = cycle(&fixture, &probe, None, 1000);
    assert_eq!(snapshot.thermal_owner, "userspace-pwm");
}

#[test]
fn simulated_hardware_roots_never_resolve_to_the_real_sysfs() {
    let fixture = prepared("sandbox-guard");
    let paths: &Paths = &fixture.paths;
    assert!(!paths.sysfs.starts_with("/sys"));
    assert!(!paths.uci_config.starts_with("/etc"));
    assert!(!paths.lock_dir.starts_with("/var/lock"));
    assert!(!paths.state_file().starts_with("/var/run"));
    let device = h5000m_fancontrol::fan::discover(paths).expect("the fixture fan");
    assert!(device.pwm_path.starts_with(&fixture.root));
}
