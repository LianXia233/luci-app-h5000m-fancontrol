//! Temperature sensor discovery and reading.
//!
//! Layering mirrors the shell controller exactly:
//! 1. Native thermal zones (CPU-ish types) and hwmon chips (CPU / Wi-Fi / PHY).
//! 2. Vendor `iwpriv` fallback for MTK radio temperature, only when the native
//!    reading is missing, bounded to one second per query.
//! 3. MT5700M manager cache with freshness check, plus the legacy QModem
//!    `/tmp/cache_*` fallback.
//!
//! A missing or invalid reading is reported as unknown (`None`) - never 0.

use crate::config::TempSource;
use crate::paths::{read_trim, sorted_glob, Paths};
use crate::proc;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where a reading came from, surfaced in the status model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SensorSource {
    ThermalZone,
    Hwmon,
    VendorIwpriv,
    ModemCache,
    LegacyCache,
    #[default]
    Missing,
}

impl SensorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SensorSource::ThermalZone => "thermal-zone",
            SensorSource::Hwmon => "hwmon",
            SensorSource::VendorIwpriv => "vendor-iwpriv",
            SensorSource::ModemCache => "modem-cache",
            SensorSource::LegacyCache => "legacy-cache",
            SensorSource::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SensorValue {
    pub temp: Option<i32>,
    pub label: String,
    pub source: SensorSource,
}

impl SensorValue {
    fn missing() -> Self {
        SensorValue {
            temp: None,
            label: String::new(),
            source: SensorSource::Missing,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SensorSnapshot {
    pub cpu: SensorValue,
    pub phy: SensorValue,
    pub wifi1: SensorValue,
    pub wifi2: SensorValue,
    pub module: SensorValue,
    pub control: Option<SensorValue>,
}

/// Vendor radio temperature query, abstracted for tests.
pub trait VendorRadio {
    fn read_temp(&mut self, iface: &str) -> Option<i32>;
}

pub struct NoVendor;

impl VendorRadio for NoVendor {
    fn read_temp(&mut self, _iface: &str) -> Option<i32> {
        None
    }
}

/// Production `iwpriv` fallback. Requires the interface to exist, `iwpriv` on
/// PATH, and answers within one second. The Rust implementation bounds the
/// wait natively, so unlike the shell version it no longer depends on the
/// external `timeout` binary.
pub struct IwprivVendor {
    pub radio_root: PathBuf,
    iwpriv: Option<PathBuf>,
    probed: bool,
}

impl IwprivVendor {
    pub fn new(radio_root: PathBuf) -> Self {
        IwprivVendor {
            radio_root,
            iwpriv: None,
            probed: false,
        }
    }
}

impl VendorRadio for IwprivVendor {
    fn read_temp(&mut self, iface: &str) -> Option<i32> {
        if !self.probed {
            self.iwpriv = proc::find_executable("iwpriv");
            self.probed = true;
        }
        self.iwpriv.as_ref()?;
        if !self.radio_root.join(iface).is_dir() {
            return None;
        }
        let program = self.iwpriv.as_ref().unwrap().to_string_lossy().to_string();
        let output = proc::run_with_timeout(&program, &[iface, "stat"], Duration::from_secs(1))?;
        parse_vendor_temperature(&output)
    }
}

/// Reproduces the awk extraction: the first line containing
/// `CurrentTemperature` followed by `=` decides; the field between the first
/// and second `=` must be a plain integer between 0 and 125.
pub fn parse_vendor_temperature(output: &str) -> Option<i32> {
    for line in output.lines() {
        let Some(marker) = line.find("CurrentTemperature") else {
            continue;
        };
        let rest = line[marker + "CurrentTemperature".len()..].trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        // awk `split($0, a, "=")` yields the text between the first two '='.
        let value = value.split('=').next().unwrap_or(value).trim();
        if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
            let parsed: i64 = value.parse().ok()?;
            if parsed <= 125 {
                return Some(parsed as i32);
            }
        }
        return None;
    }
    None
}

/// Parses a sysfs temperature. Raw values outside (-1000, 1000) are treated as
/// millidegrees and divided by 1000 (truncating, like shell arithmetic);
/// anything outside -40..150 °C is rejected.
pub fn parse_temp_c(raw: &str) -> Option<i32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut value: i64 = trimmed.parse().ok()?;
    if !(-1000..=1000).contains(&value) {
        value /= 1000;
    }
    if (-40..=150).contains(&value) {
        Some(value as i32)
    } else {
        None
    }
}

pub fn read_temp_file(path: &Path) -> Option<i32> {
    parse_temp_c(&read_trim(path)?)
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

/// CPU classification follows the ordered shell `case` patterns, so a name
/// such as `mt7996-thermal` still classifies as CPU exactly as before.
fn classify_cpu(name: &str) -> bool {
    contains_any(name, &["cpu", "soc", "mt798", "thermal"])
}

fn classify_wifi(name: &str) -> bool {
    contains_any(name, &["mt799", "wifi", "radio"])
}

fn classify_phy(name: &str) -> bool {
    contains_any(name, &["mdio", "phy"])
}

/// Reads the MT5700M manager cache (`temperature=..`, `updated=<epoch>`).
/// The first `updated=` line decides (even when malformed), matching the
/// shell `sed | head -1` extraction; the first numeric `temperature=` line
/// that satisfies the awk pattern wins.
pub fn read_module_cache(path: &Path, now: u64, max_age: u64) -> Option<i32> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut updated_raw: Option<String> = None;
    let mut temperature: Option<f64> = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // awk -F=: the field ends at the next '='.
        let value = value.split('=').next().unwrap_or(value);
        match key {
            "updated" if updated_raw.is_none() => {
                updated_raw = Some(value.to_string());
            }
            "temperature" if temperature.is_none() && is_plain_number(value) => {
                temperature = value.parse().ok();
            }
            _ => {}
        }
    }
    let updated: u64 = updated_raw?.parse().ok()?;
    let temperature = temperature?;
    let age = now.checked_sub(updated)?;
    if age > max_age {
        return None;
    }
    if !(-40.0..=150.0).contains(&temperature) {
        return None;
    }
    // awk printf %d after +/-0.5: round half away from zero.
    let rounded = if temperature < 0.0 {
        (temperature - 0.5).trunc()
    } else {
        (temperature + 0.5).trunc()
    };
    Some(rounded as i32)
}

/// Matches the awk pattern `^-?[0-9]+([.][0-9]+)?$`.
fn is_plain_number(value: &str) -> bool {
    let body = value.strip_prefix('-').unwrap_or(value);
    let mut parts = body.split('.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next();
    if parts.next().is_some() {
        return false;
    }
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match frac_part {
        None => true,
        Some(frac) => !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Extracts `"key": "temperature" ... "value": "<n>"` pairs from a legacy
/// QModem cache line and returns the first integer inside the value string.
/// Byte-oriented scanning keeps multi-byte UTF-8 content from panicking on
/// slice boundaries.
pub fn parse_legacy_cache_line(line: &str) -> Option<i32> {
    let bytes = line.as_bytes();
    let key_marker: &[u8] = b"\"key\"";
    let temp_marker: &[u8] = b"\"temperature\"";
    let mut i = 0;
    while i + key_marker.len() <= bytes.len() {
        if &bytes[i..i + key_marker.len()] == key_marker {
            let mut j = skip_ws(bytes, i + key_marker.len());
            if j < bytes.len() && bytes[j] == b':' {
                j = skip_ws(bytes, j + 1);
                if j + temp_marker.len() <= bytes.len()
                    && &bytes[j..j + temp_marker.len()] == temp_marker
                {
                    let after = j + temp_marker.len();
                    if let Some(value) = find_value_field(bytes, after) {
                        if let Some(number) = first_integer(&value) {
                            if (-40..=150).contains(&number) {
                                return Some(number);
                            }
                        }
                    }
                    return None;
                }
            }
        }
        i += 1;
    }
    None
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Finds `"value"<ws>:<ws>"..."` after `from`, with no `}` in between
/// (mirroring the `[^}]*` of the original sed expression).
fn find_value_field(bytes: &[u8], from: usize) -> Option<String> {
    let value_marker: &[u8] = b"\"value\"";
    let mut i = from;
    while i + value_marker.len() <= bytes.len() {
        if bytes[i] == b'}' {
            return None;
        }
        if &bytes[i..i + value_marker.len()] == value_marker {
            let mut j = skip_ws(bytes, i + value_marker.len());
            if j < bytes.len() && bytes[j] == b':' {
                j = skip_ws(bytes, j + 1);
                if j < bytes.len() && bytes[j] == b'"' {
                    let start = j + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end] != b'"' {
                        end += 1;
                    }
                    if end < bytes.len() {
                        return Some(String::from_utf8_lossy(&bytes[start..end]).to_string());
                    }
                    return None;
                }
            }
        }
        i += 1;
    }
    None
}

/// First integer substring (`-?[0-9]+`), equivalent to
/// `grep -Eo -- '-?[0-9]+' | head -n 1`.
fn first_integer(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let is_digit = bytes[i].is_ascii_digit();
        let sign_here = bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
        if is_digit || sign_here {
            let digits_start = if sign_here { i + 1 } else { i };
            let mut end = digits_start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let parsed: i64 = text[i..end].parse().ok()?;
            return parsed.try_into().ok();
        }
        i += 1;
    }
    None
}

fn read_legacy_module_temp(dir: &Path) -> Option<i32> {
    // Two sorted globs, base_info files first - same order as the shell loop.
    let mut candidates = sorted_glob(dir, "cache_base_info_");
    candidates.extend(sorted_glob(dir, "cache_*info*"));
    for file in candidates {
        if !file.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines() {
            if let Some(value) = parse_legacy_cache_line(line) {
                return Some(value);
            }
        }
    }
    None
}

pub struct SensorReader<'a> {
    pub paths: &'a Paths,
}

impl<'a> SensorReader<'a> {
    pub fn new(paths: &'a Paths) -> Self {
        SensorReader { paths }
    }

    pub fn read(
        &self,
        temp_source: TempSource,
        vendor: &mut dyn VendorRadio,
        now: u64,
    ) -> SensorSnapshot {
        let mut snap = SensorSnapshot {
            cpu: SensorValue::missing(),
            phy: SensorValue::missing(),
            wifi1: SensorValue::missing(),
            wifi2: SensorValue::missing(),
            module: SensorValue::missing(),
            control: None,
        };

        // Native thermal zones: first zone with a CPU-ish type wins.
        for zone in sorted_glob(
            &self.paths.sensor_root.join("class/thermal"),
            "thermal_zone",
        ) {
            let temp_path = zone.join("temp");
            if !temp_path.exists() {
                continue;
            }
            let zone_type = read_trim(&zone.join("type")).unwrap_or_default();
            let Some(temp) = read_temp_file(&temp_path) else {
                continue;
            };
            if classify_cpu(&zone_type.to_lowercase()) && snap.cpu.temp.is_none() {
                snap.cpu = SensorValue {
                    temp: Some(temp),
                    label: zone_type,
                    source: SensorSource::ThermalZone,
                };
            }
        }

        // hwmon chips, classified by the ordered shell case patterns.
        for hwmon in sorted_glob(&self.paths.sensor_root.join("class/hwmon"), "hwmon") {
            if !hwmon.is_dir() {
                continue;
            }
            let name = read_trim(&hwmon.join("name")).unwrap_or_default();
            let Some(temp) = read_temp_file(&hwmon.join("temp1_input")) else {
                continue;
            };
            let lower = name.to_lowercase();
            if classify_cpu(&lower) {
                if snap.cpu.temp.is_none() {
                    snap.cpu = SensorValue {
                        temp: Some(temp),
                        label: name.clone(),
                        source: SensorSource::Hwmon,
                    };
                }
            } else if classify_wifi(&lower) {
                let slot = if snap.wifi1.temp.is_none() {
                    Some(&mut snap.wifi1)
                } else if snap.wifi2.temp.is_none() {
                    Some(&mut snap.wifi2)
                } else {
                    None
                };
                if let Some(slot) = slot {
                    *slot = SensorValue {
                        temp: Some(temp),
                        label: name.clone(),
                        source: SensorSource::Hwmon,
                    };
                }
            } else if classify_phy(&lower) && snap.phy.temp.is_none() {
                snap.phy = SensorValue {
                    temp: Some(temp),
                    label: name.clone(),
                    source: SensorSource::Hwmon,
                };
            }
        }

        // Vendor fallback only when the native Wi-Fi reading is missing.
        if snap.wifi1.temp.is_none() {
            if let Some(temp) = vendor.read_temp("ra0") {
                snap.wifi1 = SensorValue {
                    temp: Some(temp),
                    label: "MTK radio ra0".to_string(),
                    source: SensorSource::VendorIwpriv,
                };
            }
        }
        if snap.wifi2.temp.is_none() {
            if let Some(temp) = vendor.read_temp("rai0") {
                snap.wifi2 = SensorValue {
                    temp: Some(temp),
                    label: "MTK radio rai0".to_string(),
                    source: SensorSource::VendorIwpriv,
                };
            }
        }

        // 5G module: manager cache first, legacy QModem files as fallback.
        if let Some(temp) = read_module_cache(
            &self.paths.module_cache,
            now,
            self.paths.module_cache_max_age,
        ) {
            snap.module = SensorValue {
                temp: Some(temp),
                label: "5G modem".to_string(),
                source: SensorSource::ModemCache,
            };
        } else if let Some(temp) = read_legacy_module_temp(&self.paths.legacy_cache_dir) {
            snap.module = SensorValue {
                temp: Some(temp),
                label: "5G modem".to_string(),
                source: SensorSource::LegacyCache,
            };
        }

        snap.control = select_control(&snap, temp_source);
        snap
    }
}

fn max_into(control: &mut Option<SensorValue>, candidate: &SensorValue) {
    let Some(temp) = candidate.temp else { return };
    let replace = match control {
        Some(current) => current.temp.map_or(true, |ct| temp > ct),
        None => true,
    };
    if replace {
        *control = Some(candidate.clone());
    }
}

fn select_control(snap: &SensorSnapshot, temp_source: TempSource) -> Option<SensorValue> {
    let mut control: Option<SensorValue> = None;
    max_into(&mut control, &snap.cpu);
    if temp_source == TempSource::Max {
        max_into(&mut control, &snap.phy);
        max_into(&mut control, &snap.wifi1);
        max_into(&mut control, &snap.wifi2);
        max_into(&mut control, &snap.module);
    }
    control
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // test fixtures patch a few paths
mod tests {
    use super::*;

    #[test]
    fn temp_parsing_handles_milli_and_bounds() {
        assert_eq!(parse_temp_c("61000"), Some(61));
        assert_eq!(parse_temp_c("-4500"), Some(-4)); // -4500/1000 truncates to -4
        assert_eq!(parse_temp_c("42"), Some(42));
        assert_eq!(parse_temp_c("1000"), None); // 1000 stays raw -> out of range
        assert_eq!(parse_temp_c("151"), None);
        assert_eq!(parse_temp_c("-41"), None);
        assert_eq!(parse_temp_c(""), None);
        assert_eq!(parse_temp_c("abc"), None);
        assert_eq!(parse_temp_c("--5"), None);
        assert_eq!(parse_temp_c("42\n"), Some(42));
    }

    #[test]
    fn vendor_output_parsing() {
        assert_eq!(
            parse_vendor_temperature("ra0     CurrentTemperature:42\n"),
            None // no '=' separator
        );
        assert_eq!(
            parse_vendor_temperature("CurrentTemperature = 42\nother"),
            Some(42)
        );
        assert_eq!(parse_vendor_temperature("CurrentTemperature=57"), Some(57));
        assert_eq!(parse_vendor_temperature("CurrentTemperature = 900"), None);
        assert_eq!(
            parse_vendor_temperature("CurrentTemperature = 42 junk"),
            None
        );
        assert_eq!(parse_vendor_temperature("CurrentTemperature = -3"), None);
        assert_eq!(parse_vendor_temperature("nothing"), None);
    }

    #[test]
    fn module_cache_freshness_and_rounding() {
        let tmp = std::env::temp_dir().join("h5fan-modcache-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let cache = tmp.join("temperature");
        std::fs::write(
            &cache,
            "temperature=43.6\ntemperature_sensor=modem2\nupdated=1000\n",
        )
        .unwrap();
        assert_eq!(read_module_cache(&cache, 1050, 60), Some(44));
        assert_eq!(read_module_cache(&cache, 1061, 60), None); // stale
        std::fs::write(&cache, "temperature=-10.6\nupdated=1000\n").unwrap();
        assert_eq!(read_module_cache(&cache, 1000, 60), Some(-11));
        std::fs::write(&cache, "temperature=180\nupdated=1000\n").unwrap();
        assert_eq!(read_module_cache(&cache, 1000, 60), None); // out of range
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn legacy_cache_line_parsing() {
        let line = r#"{"key": "temperature", "value": "43"}"#;
        assert_eq!(parse_legacy_cache_line(line), Some(43));
        let line = r#"{"key": "temperature", "value": "43.8 °C"}"#;
        assert_eq!(parse_legacy_cache_line(line), Some(43));
        let line = r#"{"key": "rsrp", "value": "-92"}"#;
        assert_eq!(parse_legacy_cache_line(line), None);
        let line = r#"{"key": "temperature", "value": "300"}"#;
        assert_eq!(parse_legacy_cache_line(line), None);
        let line = r#"{"key": "temperature"}, "value": "40"}"#;
        assert_eq!(parse_legacy_cache_line(line), None); // '}' blocks the match
    }

    struct StubVendor {
        values: Vec<(String, Option<i32>)>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    impl VendorRadio for StubVendor {
        fn read_temp(&mut self, iface: &str) -> Option<i32> {
            self.calls.borrow_mut().push(iface.to_string());
            self.values
                .iter()
                .find(|(name, _)| name == iface)
                .and_then(|(_, v)| *v)
        }
    }

    fn write(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn fixture_tree(tmp: &std::path::Path) -> Paths {
        let mut paths = Paths::default();
        paths.sensor_root = tmp.join("sys");
        paths.radio_root = tmp.join("net");
        paths.module_cache = tmp.join("nowhere/temperature");
        paths.legacy_cache_dir = tmp.join("nowhere-tmp");
        paths
    }

    #[test]
    fn native_sensors_classified_and_max_policy_applied() {
        let tmp = std::env::temp_dir().join("h5fan-sensor-test-1");
        let _ = std::fs::remove_dir_all(&tmp);
        write(
            &tmp.join("sys/class/thermal/thermal_zone0/type"),
            "soc_thermal\n",
        );
        write(&tmp.join("sys/class/thermal/thermal_zone0/temp"), "62000\n");
        write(&tmp.join("sys/class/hwmon/hwmon0/name"), "mt7996\n");
        write(&tmp.join("sys/class/hwmon/hwmon0/temp1_input"), "67000\n");
        write(&tmp.join("sys/class/hwmon/hwmon1/name"), "mt753x-mdio\n");
        write(&tmp.join("sys/class/hwmon/hwmon1/temp1_input"), "51000\n");
        let paths = fixture_tree(&tmp);
        let mut vendor = NoVendor;
        let snap = SensorReader::new(&paths).read(TempSource::Max, &mut vendor, 0);
        assert_eq!(snap.cpu.temp, Some(62));
        assert_eq!(snap.cpu.source, SensorSource::ThermalZone);
        assert_eq!(snap.wifi1.temp, Some(67));
        assert_eq!(snap.phy.temp, Some(51));
        let control = snap.control.unwrap();
        assert_eq!(control.temp, Some(67));
        assert_eq!(control.label, "mt7996");
        // CPU-only policy ignores Wi-Fi.
        let snap = SensorReader::new(&paths).read(TempSource::Cpu, &mut vendor, 0);
        assert_eq!(snap.control.unwrap().temp, Some(62));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn vendor_fallback_only_fills_missing_slots() {
        let tmp = std::env::temp_dir().join("h5fan-sensor-test-2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("net/ra0")).unwrap();
        std::fs::create_dir_all(tmp.join("net/rai0")).unwrap();
        let paths = fixture_tree(&tmp);
        let mut vendor = StubVendor {
            values: vec![
                ("ra0".to_string(), Some(42)),
                ("rai0".to_string(), Some(57)),
            ],
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let snap = SensorReader::new(&paths).read(TempSource::Max, &mut vendor, 0);
        assert_eq!(snap.wifi1.temp, Some(42));
        assert_eq!(snap.wifi1.source, SensorSource::VendorIwpriv);
        assert_eq!(snap.wifi2.temp, Some(57));
        assert_eq!(snap.control.as_ref().unwrap().temp, Some(57));
        // With native Wi-Fi present the vendor is not called at all.
        write(&tmp.join("sys/class/hwmon/hwmon0/name"), "wifi\n");
        write(&tmp.join("sys/class/hwmon/hwmon0/temp1_input"), "61000\n");
        write(&tmp.join("sys/class/hwmon/hwmon1/name"), "mt7996\n");
        write(&tmp.join("sys/class/hwmon/hwmon1/temp1_input"), "62000\n");
        let mut vendor = StubVendor {
            values: vec![("ra0".to_string(), Some(42))],
            calls: std::cell::RefCell::new(Vec::new()),
        };
        let snap = SensorReader::new(&paths).read(TempSource::Max, &mut vendor, 0);
        assert_eq!(snap.wifi1.temp, Some(61));
        assert_eq!(snap.wifi2.temp, Some(62));
        assert!(vendor.calls.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ordered_classification_matches_shell_case() {
        // Callers lower-case names first, matching the shell `tr A-Z a-z`.
        assert!(classify_cpu("mt7996-thermal"));
        assert!(classify_cpu("mt7986-soc"));
        assert!(classify_wifi("mt7996"));
        assert!(classify_phy("mt753x-mdio-phy"));
        // "mt7996-thermal" also contains the wifi marker, but the CPU group
        // is consulted first - exactly like the ordered shell case statement.
        assert!(classify_wifi("mt7996-thermal"));
    }

    #[test]
    fn legacy_cache_dir_scanned_when_manager_cache_absent() {
        let tmp = std::env::temp_dir().join("h5fan-sensor-test-3");
        let _ = std::fs::remove_dir_all(&tmp);
        write(
            &tmp.join("tmpdir/cache_base_info_0"),
            "{\"key\": \"temperature\", \"value\": \"44\"}\n",
        );
        let mut paths = fixture_tree(&tmp);
        paths.legacy_cache_dir = tmp.join("tmpdir");
        let mut vendor = NoVendor;
        let snap = SensorReader::new(&paths).read(TempSource::Max, &mut vendor, 0);
        assert_eq!(snap.module.temp, Some(44));
        assert_eq!(snap.module.source, SensorSource::LegacyCache);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
