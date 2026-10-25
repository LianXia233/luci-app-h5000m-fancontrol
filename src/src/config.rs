//! Effective controller configuration: defaults, validation and clamping.
//!
//! Every rule intentionally mirrors the historical shell `cfg_uint` /
//! `cfg_string` helpers, including clamping ranges, so an existing UCI config
//! keeps producing identical behaviour after the Rust migration.

use crate::uci::ConfigSource;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Manual,
    Kernel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Silent,
    Balanced,
    Performance,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempSource {
    Max,
    Cpu,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub enabled: bool,
    pub mode: Mode,
    pub profile: Profile,
    pub temp_source: TempSource,
    pub manual_pwm: u32,
    pub interval: u32,
    pub hysteresis: u32,
    pub down_delay: u32,
    pub start_pwm: u32,
    pub start_boost_ms: u32,
    pub fail_pwm: u32,
    pub curve_silent: String,
    pub curve_balanced: String,
    pub curve_performance: String,
    pub curve_custom: String,
}

pub const DEFAULT_CURVE_SILENT: &str = "20:0,35:0,40:50,55:50,70:60,85:75,95:100";
pub const DEFAULT_CURVE_BALANCED: &str = "20:0,35:0,40:50,55:60,70:75,85:90,95:100";
pub const DEFAULT_CURVE_PERFORMANCE: &str = "20:0,30:60,45:70,60:82,75:92,85:100";
pub const DEFAULT_CURVE_CUSTOM: &str = "20:0,40:50,55:60,70:75,85:90,95:100";

fn is_uint(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn cfg_uint(map: &HashMap<String, String>, option: &str, fallback: u32, min: u32, max: u32) -> u32 {
    let mut value = fallback;
    if let Some(raw) = map.get(option) {
        if is_uint(raw) {
            if let Ok(parsed) = raw.parse::<u32>() {
                value = parsed;
            }
        }
    }
    value.clamp(min, max)
}

fn cfg_string<'a>(map: &'a HashMap<String, String>, option: &str, fallback: &'a str) -> String {
    match map.get(option) {
        Some(v) if !v.is_empty() => v.clone(),
        _ => fallback.to_string(),
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            mode: Mode::Auto,
            profile: Profile::Balanced,
            temp_source: TempSource::Max,
            manual_pwm: 160,
            interval: 5,
            hysteresis: 2,
            down_delay: 30,
            start_pwm: 192,
            start_boost_ms: 700,
            fail_pwm: 255,
            curve_silent: DEFAULT_CURVE_SILENT.to_string(),
            curve_balanced: DEFAULT_CURVE_BALANCED.to_string(),
            curve_performance: DEFAULT_CURVE_PERFORMANCE.to_string(),
            curve_custom: DEFAULT_CURVE_CUSTOM.to_string(),
        }
    }
}

impl Config {
    pub fn load(source: &mut dyn ConfigSource) -> Config {
        let map = source.settings();
        Config::from_map(&map)
    }

    pub fn from_map(map: &HashMap<String, String>) -> Config {
        let mode = match cfg_string(map, "mode", "auto").as_str() {
            "manual" => Mode::Manual,
            "kernel" | "off" => Mode::Kernel,
            _ => Mode::Auto,
        };
        let profile = match cfg_string(map, "curve", "balanced").as_str() {
            "silent" => Profile::Silent,
            "performance" => Profile::Performance,
            "custom" => Profile::Custom,
            _ => Profile::Balanced,
        };
        let temp_source = match cfg_string(map, "temp_source", "max").as_str() {
            "cpu" => TempSource::Cpu,
            _ => TempSource::Max,
        };
        Config {
            enabled: cfg_uint(map, "enabled", 1, 0, 1) == 1,
            mode,
            profile,
            temp_source,
            manual_pwm: cfg_uint(map, "manual_pwm", 160, 0, 255),
            interval: cfg_uint(map, "interval", 5, 2, 60),
            hysteresis: cfg_uint(map, "hysteresis", 2, 0, 10),
            down_delay: cfg_uint(map, "down_delay", 30, 0, 300),
            start_pwm: cfg_uint(map, "start_pwm", 192, 80, 255),
            start_boost_ms: cfg_uint(map, "start_boost_ms", 700, 0, 3000),
            fail_pwm: cfg_uint(map, "fail_pwm", 255, 192, 255),
            curve_silent: cfg_string(map, "curve_silent", DEFAULT_CURVE_SILENT),
            curve_balanced: cfg_string(map, "curve_balanced", DEFAULT_CURVE_BALANCED),
            curve_performance: cfg_string(map, "curve_performance", DEFAULT_CURVE_PERFORMANCE),
            curve_custom: cfg_string(map, "curve_custom", DEFAULT_CURVE_CUSTOM),
        }
    }

    /// Curve data of the currently selected profile.
    pub fn curve_data(&self) -> &str {
        match self.profile {
            Profile::Silent => &self.curve_silent,
            Profile::Performance => &self.curve_performance,
            Profile::Custom => &self.curve_custom,
            Profile::Balanced => &self.curve_balanced,
        }
    }

    pub fn profile_curve(&self, profile: Profile) -> &str {
        match profile {
            Profile::Silent => &self.curve_silent,
            Profile::Performance => &self.curve_performance,
            Profile::Custom => &self.curve_custom,
            Profile::Balanced => &self.curve_balanced,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn defaults_match_shell_controller() {
        let cfg = Config::from_map(&HashMap::new());
        assert!(cfg.enabled);
        assert_eq!(cfg.mode, Mode::Auto);
        assert_eq!(cfg.profile, Profile::Balanced);
        assert_eq!(cfg.temp_source, TempSource::Max);
        assert_eq!(cfg.manual_pwm, 160);
        assert_eq!(cfg.interval, 5);
        assert_eq!(cfg.hysteresis, 2);
        assert_eq!(cfg.down_delay, 30);
        assert_eq!(cfg.start_pwm, 192);
        assert_eq!(cfg.start_boost_ms, 700);
        assert_eq!(cfg.fail_pwm, 255);
    }

    #[test]
    fn clamps_out_of_range_values() {
        let cfg = Config::from_map(&map(&[
            ("interval", "1"),
            ("manual_pwm", "400"),
            ("hysteresis", "99"),
            ("start_pwm", "10"),
            ("fail_pwm", "100"),
            ("start_boost_ms", "99999"),
            ("down_delay", "-5"),
        ]));
        assert_eq!(cfg.interval, 2);
        assert_eq!(cfg.manual_pwm, 255);
        assert_eq!(cfg.hysteresis, 10);
        assert_eq!(cfg.start_pwm, 80);
        assert_eq!(cfg.fail_pwm, 192);
        assert_eq!(cfg.start_boost_ms, 3000);
        // Non-numeric counts as unset: down_delay keeps its default.
        assert_eq!(cfg.down_delay, 30);
    }

    #[test]
    fn legacy_off_mode_maps_to_kernel() {
        let cfg = Config::from_map(&map(&[("mode", "off")]));
        assert_eq!(cfg.mode, Mode::Kernel);
    }

    #[test]
    fn invalid_enums_fall_back_to_defaults() {
        let cfg = Config::from_map(&map(&[
            ("mode", "turbo"),
            ("curve", "aggressive"),
            ("temp_source", "average"),
        ]));
        assert_eq!(cfg.mode, Mode::Auto);
        assert_eq!(cfg.profile, Profile::Balanced);
        assert_eq!(cfg.temp_source, TempSource::Max);
    }

    #[test]
    fn profile_selects_curve_data() {
        let cfg = Config::from_map(&map(&[
            ("curve", "performance"),
            ("curve_performance", "30:60,60:100"),
        ]));
        assert_eq!(cfg.curve_data(), "30:60,60:100");
    }
}
