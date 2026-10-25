//! Temperature sensor discovery and reading.
//!
//! Discovery order and classification rules are the ones the shell controller
//! used: native thermal zones and hwmon devices are authoritative, the MTK
//! vendor `iwpriv` query is only a bounded fallback for radios that expose no
//! hwmon file, and the MT5700M manager cache is accepted only while fresh.
//! A missing or stale reading stays "unknown" and never becomes 0 degrees.

use crate::config::{Settings, TempSource};
use crate::paths::{list_dirs, list_files, read_trimmed, Paths};
use crate::platform::{self, BoundedRun};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TEMP_MIN_C: i32 = -40;
pub const TEMP_MAX_C: i32 = 150;
/// Vendor `iwpriv stat` reports are only trusted up to this temperature; the
/// 2.x controller used the same ceiling to reject garbage registers.
pub const VENDOR_TEMP_MAX_C: i32 = 125;
pub const VENDOR_QUERY_LIMIT: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorId {
    Cpu,
    Phy,
    Wifi1,
    Wifi2,
    Module,
}

impl SensorId {
    pub fn key(self) -> &'static str {
        match self {
            SensorId::Cpu => "cpu",
            SensorId::Phy => "phy",
            SensorId::Wifi1 => "wifi1",
            SensorId::Wifi2 => "wifi2",
            SensorId::Module => "module",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            SensorId::Cpu => "CPU",
            SensorId::Phy => "Ethernet PHY",
            SensorId::Wifi1 => "Wi-Fi radio 1",
            SensorId::Wifi2 => "Wi-Fi radio 2",
            SensorId::Module => "5G modem",
        }
    }

    pub fn all() -> [SensorId; 5] {
        [
            SensorId::Cpu,
            SensorId::Phy,
            SensorId::Wifi1,
            SensorId::Wifi2,
            SensorId::Module,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorState {
    /// A valid value was read.
    Ok,
    /// No node matched this sensor at all.
    Missing,
    /// A node exists but its value is out of range or unparsable.
    Invalid,
    /// A cached value exists but is older than the accepted age.
    Stale,
    /// The optional helper failed or timed out.
    Failed,
}

impl SensorState {
    pub fn as_str(self) -> &'static str {
        match self {
            SensorState::Ok => "ok",
            SensorState::Missing => "missing",
            SensorState::Invalid => "invalid",
            SensorState::Stale => "stale",
            SensorState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorReading {
    pub id: SensorId,
    pub value_c: Option<i32>,
    pub state: SensorState,
    /// hwmon `name` or thermal zone `type`, i.e. what the kernel calls it.
    pub label: Option<String>,
    /// Machine readable origin, e.g. `thermal-zone:cpu` or `iwpriv:ra0`.
    pub source: Option<String>,
    pub path: Option<String>,
    pub error: Option<String>,
    pub sampled_at: u64,
}

impl SensorReading {
    pub fn new(id: SensorId, sampled_at: u64) -> Self {
        SensorReading {
            id,
            value_c: None,
            state: SensorState::Missing,
            label: None,
            source: None,
            path: None,
            error: None,
            sampled_at,
        }
    }

    pub fn with_value(mut self, value_c: i32, label: String, source: String, path: String) -> Self {
        self.value_c = Some(value_c);
        self.state = SensorState::Ok;
        self.label = Some(label);
        self.source = Some(source);
        self.path = Some(path);
        self
    }

    pub fn with_problem(mut self, state: SensorState, error: String) -> Self {
        self.value_c = None;
        self.state = state;
        self.error = Some(error);
        self
    }

    pub fn is_valid(&self) -> bool {
        self.state == SensorState::Ok && self.value_c.is_some()
    }
}

/// How to reach the vendor radio helper. Kept behind a trait so the parsing and
/// precedence rules can be tested without `iwpriv` or a wireless driver.
pub trait RadioProbe {
    fn helper_available(&self) -> bool;
    fn run(&self, iface: &str) -> BoundedRun;
}

pub struct IwprivProbe {
    pub limit: Duration,
}

impl Default for IwprivProbe {
    fn default() -> Self {
        IwprivProbe {
            limit: VENDOR_QUERY_LIMIT,
        }
    }
}

impl RadioProbe for IwprivProbe {
    fn helper_available(&self) -> bool {
        platform::which("iwpriv").is_some()
    }

    fn run(&self, iface: &str) -> BoundedRun {
        platform::run_bounded("iwpriv", &[iface, "stat"], self.limit)
    }
}

/// Parse one `iwpriv <iface> stat` report the way the shell awk did: the first
/// `CurrentTemperature =` line wins, and only a bare integer up to 125 is a
/// temperature. Anything else means "no reading".
pub fn parse_vendor_temperature(output: &str) -> Option<i32> {
    for line in output.lines() {
        let Some(at_marker) = line.find("CurrentTemperature") else {
            continue;
        };
        let tail = &line[at_marker + "CurrentTemperature".len()..];
        let after_marker = tail.trim_start();
        let Some(after_eq) = after_marker.strip_prefix('=') else {
            // A line merely containing the keyword but no assignment is not a
            // reading; the awk rule required `CurrentTemperature[[:space:]]*=`.
            continue;
        };
        let field = after_eq
            .trim()
            .split('=')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let value = field.parse::<i32>().ok()?;
        if value > VENDOR_TEMP_MAX_C {
            return None;
        }
        return Some(value);
    }
    None
}

/// Read a sysfs-style temperature, converting millidegrees when needed.
/// Returns `None` for unreadable or unparsable content.
pub fn read_temp_c(path: &Path) -> Option<i32> {
    let raw = read_trimmed(path)?;
    normalise_temp(&raw)
}

pub fn normalise_temp(raw: &str) -> Option<i32> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, text),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let magnitude = digits.parse::<i64>().ok()?;
    let scaled = magnitude * sign;
    // Values whose magnitude exceeds 1000 are millidegrees, exactly like the
    // shell heuristic. `1000` itself is treated as degrees and then rejected by
    // the range check, which is also what the shell did.
    let value = if !(-1000..=1000).contains(&scaled) {
        scaled / 1000
    } else {
        scaled
    };
    let value = value as i32;
    if (TEMP_MIN_C..=TEMP_MAX_C).contains(&value) {
        Some(value)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sensors {
    pub readings: Vec<SensorReading>,
}

impl Sensors {
    pub fn get(&self, id: SensorId) -> Option<&SensorReading> {
        self.readings.iter().find(|r| r.id == id)
    }

    pub fn value(&self, id: SensorId) -> Option<i32> {
        self.get(id).and_then(|r| r.value_c)
    }

    pub fn label(&self, id: SensorId) -> Option<&str> {
        self.get(id).and_then(|r| r.label.as_deref())
    }

    /// The temperature the curve is evaluated against.
    ///
    /// `cpu` uses the CPU reading only. `max` considers CPU, PHY, both radios
    /// and the modem cache in that order, and keeps the first sensor on a tie,
    /// matching the shell's strict `>` comparison.
    pub fn control(&self, settings: &Settings) -> Option<(&SensorReading, i32)> {
        let mut order = vec![SensorId::Cpu];
        if settings.temp_source == TempSource::Max {
            order.extend([
                SensorId::Phy,
                SensorId::Wifi1,
                SensorId::Wifi2,
                SensorId::Module,
            ]);
        }
        let mut best: Option<(&SensorReading, i32)> = None;
        for id in order {
            let Some(candidate) = self.get(id) else {
                continue;
            };
            if !candidate.is_valid() {
                continue;
            }
            let value = candidate.value_c.unwrap_or(i32::MIN);
            let better = match best {
                Some((_, current)) => value > current,
                None => true,
            };
            if better {
                best = Some((candidate, value));
            }
        }
        best
    }

    pub fn has_any_valid(&self) -> bool {
        self.readings.iter().any(|r| r.is_valid())
    }
}

fn lower_contains(haystack: &str, needles: &[&str]) -> bool {
    let text = haystack.to_lowercase();
    needles.iter().any(|needle| text.contains(needle))
}

const CPU_MARKERS: &[&str] = &["cpu", "soc", "mt798", "thermal"];
const RADIO_MARKERS: &[&str] = &["mt799", "wifi", "radio"];
const PHY_MARKERS: &[&str] = &["mdio", "phy"];

/// Read one sensor slot from thermal zones and hwmon devices.
pub fn sample_all(paths: &Paths, probe: &dyn RadioProbe, now: u64) -> Sensors {
    let mut cpu = SensorReading::new(SensorId::Cpu, now);
    let mut phy = SensorReading::new(SensorId::Phy, now);
    let mut wifi1 = SensorReading::new(SensorId::Wifi1, now);
    let mut wifi2 = SensorReading::new(SensorId::Wifi2, now);

    let mut cpu_invalid_note: Option<String> = None;

    for zone in list_dirs(&paths.thermal_dir(), "thermal_zone") {
        let temp_path = zone.join("temp");
        if !temp_path.exists() {
            continue;
        }
        let kind = read_trimmed(&zone.join("type")).unwrap_or_else(|| "unknown".to_string());
        if !lower_contains(&kind, CPU_MARKERS) {
            continue;
        }
        if cpu.is_valid() {
            continue;
        }
        match read_trimmed(&temp_path) {
            Some(raw) => match normalise_temp(&raw) {
                Some(value) => {
                    cpu = cpu.with_value(
                        value,
                        kind.clone(),
                        format!("thermal-zone:{kind}"),
                        temp_path.display().to_string(),
                    );
                }
                None => {
                    cpu_invalid_note = Some(format!(
                        "value {raw:?} in {} is outside {TEMP_MIN_C}..{TEMP_MAX_C} C",
                        temp_path.display()
                    ));
                }
            },
            None => {
                cpu_invalid_note = Some(format!("{} is not readable", temp_path.display()));
            }
        }
    }

    for hwmon in list_dirs(&paths.hwmon_dir(), "hwmon") {
        let name = match read_trimmed(&hwmon.join("name")) {
            Some(name) => name,
            None => continue,
        };
        let temp_path = hwmon.join("temp1_input");
        if !temp_path.exists() {
            continue;
        }
        let Some(raw) = read_trimmed(&temp_path) else {
            continue;
        };
        let Some(value) = normalise_temp(&raw) else {
            if lower_contains(&name, CPU_MARKERS) && !cpu.is_valid() {
                cpu_invalid_note = Some(format!(
                    "value {raw:?} in {} is outside {TEMP_MIN_C}..{TEMP_MAX_C} C",
                    temp_path.display()
                ));
            }
            continue;
        };
        let source = format!("hwmon:{name}");
        let path_text = temp_path.display().to_string();
        if lower_contains(&name, CPU_MARKERS) {
            if !cpu.is_valid() {
                cpu = cpu.with_value(value, name, source, path_text);
            }
        } else if lower_contains(&name, RADIO_MARKERS) {
            if !wifi1.is_valid() {
                wifi1 = wifi1.with_value(value, name, source, path_text);
            } else if !wifi2.is_valid() {
                wifi2 = wifi2.with_value(value, name, source, path_text);
            }
        } else if lower_contains(&name, PHY_MARKERS) && !phy.is_valid() {
            phy = phy.with_value(value, name, source, path_text);
        }
    }

    // Vendor mt_wifi drivers can expose radio temperature only through iwpriv.
    // Native readings stay authoritative, so the driver is queried only for the
    // slots that are still empty.
    for (slot, iface) in [(&mut wifi1, "ra0"), (&mut wifi2, "rai0")] {
        if slot.is_valid() {
            continue;
        }
        *slot = read_vendor_radio(paths, probe, iface, slot.id, now);
    }

    let module = read_module_temperature(paths, now);

    if !cpu.is_valid() {
        cpu.state = if cpu_invalid_note.is_some() {
            SensorState::Invalid
        } else {
            SensorState::Missing
        };
        cpu.error = cpu_invalid_note;
    }

    Sensors {
        readings: vec![cpu, phy, wifi1, wifi2, module],
    }
}

fn read_vendor_radio(
    paths: &Paths,
    probe: &dyn RadioProbe,
    iface: &str,
    id: SensorId,
    now: u64,
) -> SensorReading {
    let base = SensorReading::new(id, now);
    let interface_path = paths.radio.join(iface);
    if !interface_path.is_dir() {
        return base.with_problem(
            SensorState::Missing,
            format!("radio interface {iface} is not present"),
        );
    }
    if !probe.helper_available() {
        // An optional tool being absent is a documented degradation, not an
        // error the operator has to fix.
        return base.with_problem(
            SensorState::Missing,
            format!("iwpriv is not installed; {iface} temperature unavailable"),
        );
    }
    match probe.run(iface) {
        BoundedRun::Completed(output) => match parse_vendor_temperature(&output) {
            Some(value) => base.with_value(
                value,
                format!("MTK radio {iface}"),
                format!("iwpriv:{iface}"),
                interface_path.display().to_string(),
            ),
            None => base.with_problem(
                SensorState::Invalid,
                format!("iwpriv {iface} stat has no usable CurrentTemperature"),
            ),
        },
        BoundedRun::TimedOut => base.with_problem(
            SensorState::Failed,
            format!("iwpriv {iface} stat timed out after 1 s"),
        ),
        BoundedRun::Failed => {
            base.with_problem(SensorState::Failed, format!("iwpriv {iface} stat failed"))
        }
    }
}

/// Round a half-value away from zero, matching `printf "%d"` after `+0.5`.
/// `printf` truncates toward zero, so -1.1 becomes -1 rather than -2.
fn round_away_from_zero(value: f64) -> i32 {
    if value < 0.0 {
        (value - 0.5).trunc() as i32
    } else {
        (value + 0.5).trunc() as i32
    }
}

fn read_module_temperature(paths: &Paths, now: u64) -> SensorReading {
    let base = SensorReading::new(SensorId::Module, now);
    let cache = &paths.module_cache;
    if cache.is_file() {
        let text = match std::fs::read_to_string(cache) {
            Ok(text) => text,
            Err(_) => {
                return base.with_problem(
                    SensorState::Failed,
                    format!("{} is not readable", cache.display()),
                );
            }
        };
        let mut updated: Option<u64> = None;
        let mut temperature: Option<f64> = None;
        for line in text.lines() {
            // The manager writes `key=value`; only an exact `temperature` key is
            // a reading, `temperature_sensor` is metadata.
            let Some(field) = line.split_once('=') else {
                continue;
            };
            match field.0.trim() {
                "updated" => updated = field.1.trim().parse::<u64>().ok(),
                "temperature" => temperature = field.1.trim().parse::<f64>().ok(),
                _ => {}
            }
        }
        let (Some(updated), Some(temperature)) = (updated, temperature) else {
            return base.with_problem(
                SensorState::Invalid,
                format!("{} has no usable temperature/updated pair", cache.display()),
            );
        };
        if updated > now {
            return base.with_problem(
                SensorState::Stale,
                format!(
                    "{} timestamp {:?} s in the future",
                    cache.display(),
                    updated - now
                ),
            );
        }
        let age = now - updated;
        if age > paths.module_cache_max_age {
            return base.with_problem(
                SensorState::Stale,
                format!(
                    "{} is {age} s old, limit is {} s",
                    cache.display(),
                    paths.module_cache_max_age
                ),
            );
        }
        if !(TEMP_MIN_C as f64..=TEMP_MAX_C as f64).contains(&temperature) {
            return base.with_problem(
                SensorState::Invalid,
                format!("cached temperature {temperature} is out of range"),
            );
        }
        return base.with_value(
            round_away_from_zero(temperature),
            "5G modem".to_string(),
            "modem-cache".to_string(),
            cache.display().to_string(),
        );
    }

    // Legacy QModem style JSON snippets in /tmp.
    let mut candidates = list_files(&paths.legacy_cache_dir, "cache_base_info_");
    candidates.extend(list_files(&paths.legacy_cache_dir, "cache_"));
    candidates.retain(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .map(|name| name.starts_with("cache_") && name.contains("info"))
            .unwrap_or(false)
    });
    let mut deduped: Vec<PathBuf> = Vec::new();
    for path in candidates {
        if !deduped.contains(&path) {
            deduped.push(path);
        }
    }
    for path in deduped {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(raw) = extract_vendor_json_temperature(&text) else {
            continue;
        };
        let Some(value) =
            first_signed_integer(&raw).filter(|number| (TEMP_MIN_C..=TEMP_MAX_C).contains(number))
        else {
            continue;
        };
        return base.with_value(
            value,
            "5G modem".to_string(),
            "legacy-cache".to_string(),
            path.display().to_string(),
        );
    }
    base.with_problem(
        SensorState::Missing,
        "no modem temperature cache".to_string(),
    )
}

/// Pull the quoted `value` that follows a `"key":"temperature"` member, stopping
/// at the end of the enclosing object like the original sed expression did.
pub fn extract_vendor_json_temperature(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(value) = extract_from_line(line) {
            return Some(value);
        }
    }
    None
}

fn extract_from_line(line: &str) -> Option<String> {
    let mut search = line;
    loop {
        let key_at = search.find("\"key\"")?;
        let after_key = search[key_at + "\"key\"".len()..].trim_start();
        let Some(after_colon) = after_key.strip_prefix(':') else {
            search = &search[key_at + 1..];
            continue;
        };
        let after_colon = after_colon.trim_start();
        let Some(after_name) = after_colon.strip_prefix("\"temperature\"") else {
            search = &search[key_at + 1..];
            continue;
        };
        // The original pattern used `[^}]*`, so the value member has to live in
        // the same object.
        let segment = match after_name.find('}') {
            Some(end) => &after_name[..end],
            None => after_name,
        };
        let value_at = segment.find("\"value\"")?;
        let after_value = segment[value_at + "\"value\"".len()..].trim_start();
        let after_colon = after_value.strip_prefix(':')?.trim_start();
        let mut chars = after_colon.chars();
        if chars.next()? != '"' {
            return None;
        }
        let captured: String = chars.take_while(|c| *c != '"').collect();
        return Some(captured);
    }
}

/// Leftmost `-?[0-9]+` match, which is what `grep -Eo | head -1` selected.
pub fn first_signed_integer(text: &str) -> Option<i32> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let mut cursor = index;
        if bytes[cursor] == b'-' {
            cursor += 1;
        }
        let digits_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > digits_start {
            let token = &text[start..cursor];
            if let Ok(value) = token.parse::<i32>() {
                return Some(value);
            }
            index = cursor;
            continue;
        }
        index = start + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degree_and_millidegree_readings_normalise() {
        assert_eq!(normalise_temp("56000"), Some(56));
        assert_eq!(normalise_temp("56000\n"), Some(56));
        assert_eq!(normalise_temp("56"), Some(56));
        assert_eq!(normalise_temp("150"), Some(150));
        assert_eq!(normalise_temp("-40"), Some(-40));
        assert_eq!(normalise_temp("-15000"), Some(-15));
        assert_eq!(normalise_temp("1000"), None, "1000 C is not a temperature");
        assert_eq!(normalise_temp("2500"), Some(2));
    }

    #[test]
    fn garbage_and_out_of_range_are_rejected_not_zeroed() {
        for raw in [
            "",
            "  ",
            "abc",
            "56.7",
            "--56",
            "5 6",
            "+56",
            "100000000000000000000000000",
        ] {
            assert_eq!(normalise_temp(raw), None, "{raw:?} must not be a reading");
        }
        assert_eq!(
            normalise_temp("200000"),
            None,
            "200000 mK is 200 C, above the accepted range"
        );
        assert_eq!(normalise_temp("-200000"), None);
        assert_eq!(normalise_temp("160"), None, "above 150 C");
        assert_eq!(normalise_temp("-41"), None, "below -40 C");
    }

    #[test]
    fn vendor_report_parsing_is_exact() {
        assert_eq!(
            parse_vendor_temperature("CurrentTemperature = 42"),
            Some(42)
        );
        assert_eq!(parse_vendor_temperature("CurrentTemperature=42"), Some(42));
        assert_eq!(
            parse_vendor_temperature("ra0\tCurrentTemperature = 57  "),
            Some(57)
        );
        // Trailing junk on the same field is not a temperature; the shell awk
        // anchored the field with ^[0-9]+$ and rejected it too.
        assert_eq!(
            parse_vendor_temperature("ra0\tCurrentTemperature = 57\tnote"),
            None
        );
        assert_eq!(
            parse_vendor_temperature("CurrentTemperature = 125"),
            Some(125)
        );
        assert_eq!(parse_vendor_temperature("CurrentTemperature = 126"), None);
        assert_eq!(parse_vendor_temperature("CurrentTemperature = 900"), None);
        assert_eq!(
            parse_vendor_temperature("CurrentTemperature = 42 junk"),
            None
        );
        assert_eq!(parse_vendor_temperature("CurrentTemperature = -5"), None);
        assert_eq!(parse_vendor_temperature("CurrentTemperature 42"), None);
        assert_eq!(parse_vendor_temperature("no temperature here"), None);
        assert_eq!(parse_vendor_temperature(""), None);
    }

    #[test]
    fn vendor_report_first_line_wins_even_when_invalid() {
        // The shell awk used `exit` after the first matching line, so a broken
        // first report must not be masked by a later good one.
        let output = "CurrentTemperature = 900\nCurrentTemperature = 42\n";
        assert_eq!(parse_vendor_temperature(output), None);
    }

    #[test]
    fn vendor_report_field_stops_at_the_second_equals_sign() {
        // `split($0, a, "=")` in the shell took the text between the first two
        // equals signs, so a trailing `= 99` is ignored rather than parsed.
        assert_eq!(
            parse_vendor_temperature("CurrentTemperature = 42 = 99"),
            Some(42)
        );
    }

    #[test]
    fn modem_cache_rounds_away_from_zero() {
        assert_eq!(round_away_from_zero(43.6), 44);
        assert_eq!(round_away_from_zero(43.4), 43);
        assert_eq!(round_away_from_zero(43.5), 44);
        assert_eq!(round_away_from_zero(-0.6), -1);
        assert_eq!(round_away_from_zero(-0.4), 0);
    }

    #[test]
    fn legacy_json_member_is_extracted() {
        let line = r#"{"key" : "temperature", "unit":"C", "value" : "43.6" }"#;
        assert_eq!(
            extract_vendor_json_temperature(line).as_deref(),
            Some("43.6")
        );
        assert_eq!(first_signed_integer("43.6"), Some(43));
        assert_eq!(first_signed_integer("-12"), Some(-12));
        assert_eq!(first_signed_integer("abc-12def"), Some(-12));
        assert_eq!(first_signed_integer("no digits"), None);
        assert_eq!(first_signed_integer("-x9"), Some(9));
    }

    #[test]
    fn legacy_json_member_must_be_in_the_same_object() {
        let split = r#"{"key":"temperature"}{"value":"43"}"#;
        assert_eq!(extract_vendor_json_temperature(split), None);
        let wrong_key = r#"{"key":"rssi","value":"43"}{"key":"temperature","value":"44"}"#;
        assert_eq!(
            extract_vendor_json_temperature(wrong_key).as_deref(),
            Some("44")
        );
    }

    #[test]
    fn sensor_states_round_trip_as_text() {
        assert_eq!(SensorState::Stale.as_str(), "stale");
        assert_eq!(SensorId::Module.key(), "module");
        assert_eq!(SensorId::all().len(), 5);
    }

    #[test]
    fn classification_markers_match_the_shell_patterns() {
        assert!(lower_contains("mt7981_thermal", CPU_MARKERS));
        assert!(lower_contains("cpu-thermal", CPU_MARKERS));
        assert!(lower_contains("ThermalZone", CPU_MARKERS));
        assert!(lower_contains("mt7996_2g", RADIO_MARKERS));
        assert!(lower_contains("wifi5g", RADIO_MARKERS));
        assert!(lower_contains("mtk-mdio-phy", PHY_MARKERS));
        assert!(!lower_contains("pwmfan", CPU_MARKERS));
        assert!(!lower_contains("pwmfan", RADIO_MARKERS));
    }

    #[test]
    fn control_selection_prefers_the_first_sensor_on_a_tie() {
        let now = 1000;
        let cpu = SensorReading::new(SensorId::Cpu, now).with_value(
            60,
            "cpu-thermal".into(),
            "thermal-zone:cpu".into(),
            "/x".into(),
        );
        let phy = SensorReading::new(SensorId::Phy, now).with_value(
            60,
            "mtk-mdio-phy".into(),
            "hwmon:phy".into(),
            "/y".into(),
        );
        let sensors = Sensors {
            readings: vec![cpu, phy],
        };
        let settings = Settings::default();
        let (reading, value) = sensors.control(&settings).expect("a control temperature");
        assert_eq!(value, 60);
        assert_eq!(reading.id, SensorId::Cpu);
    }

    #[test]
    fn cpu_source_ignores_other_sensors() {
        let now = 1000;
        let cpu = SensorReading::new(SensorId::Cpu, now).with_value(
            50,
            "cpu".into(),
            "thermal-zone:cpu".into(),
            "/x".into(),
        );
        let module = SensorReading::new(SensorId::Module, now).with_value(
            80,
            "5G modem".into(),
            "modem-cache".into(),
            "/y".into(),
        );
        let sensors = Sensors {
            readings: vec![cpu, module],
        };
        let cpu_source = Settings {
            temp_source: TempSource::Cpu,
            ..Settings::default()
        };
        assert_eq!(
            sensors.control(&cpu_source).map(|(_, value)| value),
            Some(50)
        );
        let max_source = Settings {
            temp_source: TempSource::Max,
            ..Settings::default()
        };
        assert_eq!(
            sensors.control(&max_source).map(|(_, value)| value),
            Some(80)
        );
    }

    #[test]
    fn invalid_and_stale_readings_are_never_control_inputs() {
        let now = 1000;
        let cpu = SensorReading::new(SensorId::Cpu, now)
            .with_problem(SensorState::Invalid, "out of range".into());
        let module = SensorReading::new(SensorId::Module, now)
            .with_problem(SensorState::Stale, "90 s old".into());
        let sensors = Sensors {
            readings: vec![cpu, module],
        };
        let settings = Settings::default();
        assert!(sensors.control(&settings).is_none());
        assert!(!sensors.has_any_valid());
    }
}
