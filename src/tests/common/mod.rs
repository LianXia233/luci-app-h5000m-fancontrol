//! Shared fixture that builds a simulated H5000M environment.
//!
//! Every file the controller reads lives under one temporary directory, so the
//! regression tests can drive real control cycles without ever touching a PWM
//! register, a thermal zone or the host configuration.

#![allow(dead_code)]

use h5000m_fancontrol::paths::Paths;
use h5000m_fancontrol::platform::BoundedRun;
use h5000m_fancontrol::sensor::RadioProbe;
use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub paths: Paths,
    hwmon_index: Cell<usize>,
    cooling_index: Cell<usize>,
    zone_index: Cell<usize>,
}

/// A radio helper whose answers are scripted, so the vendor fallback can be
/// tested without `iwpriv` or a wireless driver.
pub struct StubProbe {
    pub answers: HashMap<String, BoundedRun>,
    pub helper: bool,
    pub calls: std::cell::RefCell<Vec<String>>,
}

impl StubProbe {
    pub fn new() -> Self {
        StubProbe {
            answers: HashMap::new(),
            helper: true,
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub fn answering(mut self, iface: &str, text: &str) -> Self {
        self.answers
            .insert(iface.to_string(), BoundedRun::Completed(text.to_string()));
        self
    }

    pub fn timing_out(mut self, iface: &str) -> Self {
        self.answers.insert(iface.to_string(), BoundedRun::TimedOut);
        self
    }

    pub fn failing(mut self, iface: &str) -> Self {
        self.answers.insert(iface.to_string(), BoundedRun::Failed);
        self
    }

    pub fn without_helper(mut self) -> Self {
        self.helper = false;
        self
    }

    pub fn call_count(&self) -> usize {
        self.calls.borrow().len()
    }
}

impl Default for StubProbe {
    fn default() -> Self {
        StubProbe::new()
    }
}

impl RadioProbe for StubProbe {
    fn helper_available(&self) -> bool {
        self.helper
    }

    fn run(&self, iface: &str) -> BoundedRun {
        self.calls.borrow_mut().push(iface.to_string());
        self.answers
            .get(iface)
            .cloned()
            .unwrap_or(BoundedRun::Failed)
    }
}

pub fn write(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, contents).expect("write file");
}

pub fn be32(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_be_bytes());
    }
    out
}

impl Fixture {
    pub fn new(label: &str) -> Fixture {
        let sequence = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "h5fan-it-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::sandbox(&root);
        std::fs::create_dir_all(paths.thermal_dir()).expect("thermal dir");
        std::fs::create_dir_all(paths.hwmon_dir()).expect("hwmon dir");
        std::fs::create_dir_all(&paths.run_dir).expect("run dir");
        std::fs::create_dir_all(&paths.proc_root).expect("proc dir");
        std::fs::create_dir_all(paths.uci_config.parent().unwrap()).expect("etc dir");
        std::fs::create_dir_all(&paths.legacy_cache_dir).expect("tmp dir");
        Fixture {
            hwmon_index: Cell::new(1),
            cooling_index: Cell::new(0),
            zone_index: Cell::new(0),
            root,
            paths,
        }
    }

    /// A thermal zone with a temperature, a governor and its policy list.
    pub fn thermal_zone(&self, kind: &str, temp_millideg: i64) -> &Fixture {
        let index = self.zone_index.get();
        self.zone_index.set(index + 1);
        let zone = self
            .paths
            .thermal_dir()
            .join(format!("thermal_zone{index}"));
        write(&zone.join("type"), format!("{kind}\n"));
        write(&zone.join("temp"), format!("{temp_millideg}\n"));
        write(&zone.join("policy"), "step_wise\n");
        write(&zone.join("available_policies"), "step_wise user_space\n");
        self
    }

    pub fn thermal_zone_path(&self, index: usize) -> PathBuf {
        self.paths
            .thermal_dir()
            .join(format!("thermal_zone{index}"))
    }

    /// An hwmon chip, optionally with a temperature sensor.
    pub fn hwmon(&self, name: &str, temp_millideg: Option<i64>) -> PathBuf {
        let index = self.hwmon_index.get();
        self.hwmon_index.set(index + 1);
        let dir = self.paths.hwmon_dir().join(format!("hwmon{index}"));
        write(&dir.join("name"), format!("{name}\n"));
        if let Some(value) = temp_millideg {
            write(&dir.join("temp1_input"), format!("{value}\n"));
        }
        dir
    }

    /// The case fan: an hwmon chip called `pwmfan` with a writable `pwm1`.
    pub fn pwm_fan(&self, initial_pwm: u8, rpm: Option<u32>) -> PathBuf {
        let dir = self.hwmon("pwmfan", None);
        write(&dir.join("pwm1"), format!("{initial_pwm}\n"));
        write(&dir.join("pwm1_max"), "255\n");
        if let Some(rpm) = rpm {
            write(&dir.join("fan1_input"), format!("{rpm}\n"));
        }
        dir
    }

    pub fn pwm_fan_cooling_device(&self, max_state: u32) -> PathBuf {
        let index = self.cooling_index.get();
        self.cooling_index.set(index + 1);
        let dir = self
            .paths
            .thermal_dir()
            .join(format!("cooling_device{index}"));
        write(&dir.join("type"), "pwm-fan\n");
        write(&dir.join("max_state"), format!("{max_state}\n"));
        write(&dir.join("cur_state"), "0\n");
        dir
    }

    pub fn cpufreq_cooling_device(&self) -> PathBuf {
        let index = self.cooling_index.get();
        self.cooling_index.set(index + 1);
        let dir = self
            .paths
            .thermal_dir()
            .join(format!("cooling_device{index}"));
        write(&dir.join("type"), "cpufreq\n");
        write(&dir.join("max_state"), "13\n");
        dir
    }

    /// The `pwm-fan` devicetree node: its phandle and the cooling level table.
    pub fn dt_pwm_fan(&self, phandle: u32, cooling_levels: &[u32]) -> &Fixture {
        let dir = self.paths.pwm_fan_dt();
        write(&dir.join("phandle"), be32(&[phandle]));
        write(&dir.join("cooling-levels"), be32(cooling_levels));
        self
    }

    /// A devicetree trip point. The node name has to start with `trip-point`,
    /// which is how the controller enumerates them.
    pub fn dt_trip(
        &self,
        zone: &str,
        name: &str,
        kind: &str,
        temp_millideg: u32,
        hysteresis_millideg: u32,
        phandle: u32,
    ) -> &Fixture {
        let dir = self
            .paths
            .cooling_map_root()
            .join(zone)
            .join("trips")
            .join(name);
        write(&dir.join("type"), format!("{kind}\n"));
        write(&dir.join("temperature"), be32(&[temp_millideg]));
        write(&dir.join("hysteresis"), be32(&[hysteresis_millideg]));
        write(&dir.join("phandle"), be32(&[phandle]));
        self
    }

    /// A cooling map binding a trip to a cooling device phandle and state.
    pub fn dt_cooling_map(
        &self,
        zone: &str,
        name: &str,
        device_phandle: u32,
        state: u32,
        trip_phandle: u32,
    ) -> &Fixture {
        let dir = self
            .paths
            .cooling_map_root()
            .join(zone)
            .join("cooling-maps")
            .join(name);
        write(&dir.join("cooling-device"), be32(&[device_phandle, state]));
        write(&dir.join("trip"), be32(&[trip_phandle]));
        self
    }

    pub fn uci(&self, text: &str) -> &Fixture {
        write(&self.paths.uci_config, text);
        self
    }

    pub fn uci_defaults(&self) -> &Fixture {
        self.uci("config settings 'settings'\n\toption mode 'auto'\n\toption curve 'balanced'\n")
    }

    pub fn modem_cache(&self, temperature: &str, updated: u64) {
        write(
            &self.paths.module_cache,
            format!("temperature={temperature}\ntemperature_sensor=modem2\nupdated={updated}\n"),
        );
    }

    pub fn radio_interface(&self, name: &str) {
        std::fs::create_dir_all(self.paths.radio.join(name)).expect("radio interface dir");
    }

    pub fn read_pwm(&self, dir: &Path) -> Option<u8> {
        std::fs::read_to_string(dir.join("pwm1"))
            .ok()?
            .trim()
            .parse::<u8>()
            .ok()
    }

    pub fn pwm_path(&self, dir: &Path) -> PathBuf {
        dir.join("pwm1")
    }

    pub fn read_policy(&self, zone: &Path) -> Option<String> {
        std::fs::read_to_string(zone.join("policy"))
            .ok()
            .map(|value| value.trim().to_string())
    }

    pub fn state_text(&self) -> Option<String> {
        std::fs::read_to_string(self.paths.state_file()).ok()
    }

    pub fn publish_state(&self, text: &str) {
        write(&self.paths.state_file(), text);
    }

    pub fn fake_proc(&self, pid: u32, cmdline: &str) {
        let joined = cmdline.split(' ').collect::<Vec<_>>().join("\0");
        write(
            &self.paths.proc_root.join(pid.to_string()).join("cmdline"),
            joined.as_bytes(),
        );
    }

    /// Make liveness checks see this test process as a running controller.
    pub fn touch_own_proc(&self) -> u32 {
        let pid = std::process::id();
        self.fake_proc(pid, "/usr/sbin/h5000m-fancontrol daemon");
        pid
    }

    pub fn hwmon_dir(&self, index: usize) -> PathBuf {
        self.paths.hwmon_dir().join(format!("hwmon{index}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Millidegrees helper, so a test reads like a temperature.
pub fn mc(value: i32) -> i64 {
    value as i64 * 1000
}
