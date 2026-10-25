//! Typed, validated view of `/etc/config/h5000m_fancontrol`.
//!
//! Every option keeps the exact range and fallback of the shell controller it
//! replaces, and each correction is recorded as a [`ConfigIssue`] so the
//! diagnostics tab can show what the operator should fix instead of silently
//! controlling with a substituted value.

use crate::curve::{self, CurveError, CurvePoint};
use crate::error::{Error, Result};
use crate::paths::Paths;
use crate::uci::UciFile;
use std::fmt;

pub const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto,
    Manual,
    Kernel,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Manual => "manual",
            Mode::Kernel => "kernel",
        }
    }

    /// `off` was the 1.x name for what is now kernel-only protection. The second
    /// tuple element is a migration note for the diagnostics tab.
    pub fn parse(raw: &str) -> Option<(Mode, Option<&'static str>)> {
        match raw {
            "auto" => Some((Mode::Auto, None)),
            "manual" => Some((Mode::Manual, None)),
            "kernel" => Some((Mode::Kernel, None)),
            "off" => Some((
                Mode::Kernel,
                Some("legacy value 'off' was mapped to 'kernel'"),
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Silent,
    Balanced,
    Performance,
    Custom,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Silent => "silent",
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
            Profile::Custom => "custom",
        }
    }

    pub fn parse(raw: &str) -> Option<Profile> {
        match raw {
            "silent" => Some(Profile::Silent),
            "balanced" => Some(Profile::Balanced),
            "performance" => Some(Profile::Performance),
            "custom" => Some(Profile::Custom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TempSource {
    /// Hottest valid sensor, which is what the device needs for shared cooling.
    Max,
    /// CPU only.
    Cpu,
}

impl TempSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TempSource::Max => "max",
            TempSource::Cpu => "cpu",
        }
    }

    pub fn parse(raw: &str) -> Option<TempSource> {
        match raw {
            "max" => Some(TempSource::Max),
            "cpu" => Some(TempSource::Cpu),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub option: String,
    pub value: String,
    pub problem: String,
    pub applied: String,
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}={:?}: {} (using {})",
            self.option, self.value, self.problem, self.applied
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub schema_version: u32,
    pub enabled: bool,
    pub mode: Mode,
    pub profile: Profile,
    pub temp_source: TempSource,
    pub manual_pwm: u8,
    pub interval: u64,
    pub hysteresis: i32,
    pub down_delay: u64,
    pub start_pwm: u8,
    pub start_boost_ms: u64,
    pub fail_pwm: u8,
    pub curve_silent: String,
    pub curve_balanced: String,
    pub curve_performance: String,
    pub curve_custom: String,
    pub issues: Vec<ConfigIssue>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            schema_version: SCHEMA_VERSION,
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
            curve_silent: curve::SILENT.to_string(),
            curve_balanced: curve::BALANCED.to_string(),
            curve_performance: curve::PERFORMANCE.to_string(),
            curve_custom: curve::CUSTOM.to_string(),
            issues: Vec::new(),
        }
    }
}

/// Accumulates the corrections applied while reading the configuration.
#[derive(Default)]
struct Issues(Vec<ConfigIssue>);

impl Issues {
    fn add(&mut self, option: &str, value: &str, problem: String, applied: String) {
        self.0.push(ConfigIssue {
            option: option.to_string(),
            value: value.to_string(),
            problem,
            applied,
        });
    }
}

impl Settings {
    pub fn load(paths: &Paths) -> Result<Settings> {
        match UciFile::read(paths) {
            Ok(file) => Ok(Settings::from_uci(&file)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let mut settings = Settings::default();
                settings.issues.push(ConfigIssue {
                    option: "(file)".to_string(),
                    value: String::new(),
                    problem: format!(
                        "configuration file missing at {}",
                        paths.uci_config.display()
                    ),
                    applied: "built-in defaults".to_string(),
                });
                Ok(settings)
            }
            Err(err) => Err(Error::io(paths.uci_config.display().to_string(), &err)),
        }
    }

    pub fn from_uci(file: &UciFile) -> Settings {
        let default = Settings::default();
        let mut settings = default.clone();
        settings.issues.clear();
        let mut issues = Issues::default();

        settings.schema_version = match file.setting("schema_version") {
            // A missing marker only means the config predates schema tracking;
            // it is not something the operator has to fix.
            None => SCHEMA_VERSION,
            Some(raw) => match parse_uint_str(raw) {
                Some(value) => value as u32,
                None => {
                    issues.add(
                        "schema_version",
                        raw,
                        "not a non-negative integer".to_string(),
                        format!("assumed {SCHEMA_VERSION}"),
                    );
                    SCHEMA_VERSION
                }
            },
        };

        settings.enabled = match file.setting("enabled") {
            None => default.enabled,
            Some(raw) => match parse_uint_str(raw) {
                Some(value) => value.clamp(0, 1) != 0,
                None => {
                    issues.add(
                        "enabled",
                        raw,
                        "not a non-negative integer".to_string(),
                        "1 (enabled)".to_string(),
                    );
                    true
                }
            },
        };

        settings.mode = match file.setting("mode") {
            None => default.mode,
            Some(raw) => match Mode::parse(raw) {
                Some((mode, note)) => {
                    if let Some(note) = note {
                        issues.add(
                            "mode",
                            raw,
                            note.to_string(),
                            format!("{} (safe default)", mode.as_str()),
                        );
                    }
                    mode
                }
                None => {
                    issues.add(
                        "mode",
                        raw,
                        "unknown mode".to_string(),
                        "auto (safe default)".to_string(),
                    );
                    Mode::Auto
                }
            },
        };

        settings.profile = match file.setting("curve") {
            None => default.profile,
            Some(raw) => match Profile::parse(raw) {
                Some(profile) => profile,
                None => {
                    issues.add(
                        "curve",
                        raw,
                        "unknown curve profile".to_string(),
                        "balanced (safe default)".to_string(),
                    );
                    Profile::Balanced
                }
            },
        };

        settings.temp_source = match file.setting("temp_source") {
            None => default.temp_source,
            Some(raw) => match TempSource::parse(raw) {
                Some(source) => source,
                None => {
                    issues.add(
                        "temp_source",
                        raw,
                        "unknown temperature source".to_string(),
                        "max (safe default)".to_string(),
                    );
                    TempSource::Max
                }
            },
        };

        settings.manual_pwm = uint_option(file, "manual_pwm", 160, 0, 255, &mut issues) as u8;
        settings.interval = uint_option(file, "interval", 5, 2, 60, &mut issues) as u64;
        settings.hysteresis = uint_option(file, "hysteresis", 2, 0, 10, &mut issues) as i32;
        settings.down_delay = uint_option(file, "down_delay", 30, 0, 300, &mut issues) as u64;
        settings.start_pwm = uint_option(file, "start_pwm", 192, 80, 255, &mut issues) as u8;
        settings.start_boost_ms =
            uint_option(file, "start_boost_ms", 700, 0, 3000, &mut issues) as u64;
        // fail_pwm has to out-rank every other output; 192..255 is the range the
        // shell controller used and it keeps the fan above 75 %.
        settings.fail_pwm = uint_option(file, "fail_pwm", 255, 192, 255, &mut issues) as u8;

        settings.curve_silent = string_option(file, "curve_silent", curve::SILENT, &mut issues);
        settings.curve_balanced =
            string_option(file, "curve_balanced", curve::BALANCED, &mut issues);
        settings.curve_performance =
            string_option(file, "curve_performance", curve::PERFORMANCE, &mut issues);
        settings.curve_custom = string_option(file, "curve_custom", curve::CUSTOM, &mut issues);

        settings.issues = issues.0;
        settings
    }

    pub fn curve_spec(&self) -> &str {
        match self.profile {
            Profile::Silent => &self.curve_silent,
            Profile::Performance => &self.curve_performance,
            Profile::Custom => &self.curve_custom,
            Profile::Balanced => &self.curve_balanced,
        }
    }

    pub fn active_curve(&self) -> std::result::Result<Vec<CurvePoint>, CurveError> {
        curve::parse(self.curve_spec())
    }

    /// Description of why the curve cannot be used, if it cannot.
    pub fn curve_failure(&self) -> Option<String> {
        self.active_curve().err().map(|err| err.to_string())
    }

    pub fn is_kernel_only(&self) -> bool {
        !self.enabled || self.mode == Mode::Kernel
    }

    /// Options that change how the fan is driven. Used by the frontend to decide
    /// whether a save needs an apply cycle at all.
    pub fn policy_signature(&self) -> String {
        format!(
            "v1|enabled={}|mode={}|profile={}|temp_source={}|manual_pwm={}|hysteresis={}|down_delay={}|start_pwm={}|start_boost_ms={}|fail_pwm={}|interval={}|curve={}",
            self.enabled,
            self.mode.as_str(),
            self.profile.as_str(),
            self.temp_source.as_str(),
            self.manual_pwm,
            self.hysteresis,
            self.down_delay,
            self.start_pwm,
            self.start_boost_ms,
            self.fail_pwm,
            self.interval,
            self.curve_spec(),
        )
    }
}

fn parse_uint_str(raw: &str) -> Option<i64> {
    let text = raw.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // A digit string too large for i64 is still "out of range", not garbage,
    // so saturate and let the caller clamp.
    match text.parse::<i64>() {
        Ok(value) => Some(value),
        Err(_) => Some(i64::MAX),
    }
}

fn uint_option(
    file: &UciFile,
    option: &str,
    fallback: i64,
    min: i64,
    max: i64,
    issues: &mut Issues,
) -> i64 {
    let Some(raw) = file.setting(option) else {
        return fallback;
    };
    match parse_uint_str(raw) {
        Some(value) if value < min => {
            issues.add(
                option,
                raw,
                format!("below the minimum of {min}"),
                format!("clamped to {min}"),
            );
            min
        }
        Some(value) if value > max => {
            issues.add(
                option,
                raw,
                format!("above the maximum of {max}"),
                format!("clamped to {max}"),
            );
            max
        }
        Some(value) => value,
        None => {
            issues.add(
                option,
                raw,
                "not a non-negative integer".to_string(),
                format!("default {fallback}"),
            );
            fallback
        }
    }
}

fn string_option(file: &UciFile, option: &str, fallback: &str, issues: &mut Issues) -> String {
    match file.setting(option) {
        None => fallback.to_string(),
        Some(raw) if raw.trim().is_empty() => {
            issues.add(
                option,
                raw,
                "empty value".to_string(),
                format!("default {fallback}"),
            );
            fallback.to_string()
        }
        Some(raw) => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(text: &str) -> Settings {
        Settings::from_uci(&UciFile::parse(text))
    }

    #[test]
    fn defaults_match_the_released_package() {
        let settings = settings("config settings 'settings'\n");
        assert_eq!(settings, Settings::default());
        assert!(settings.issues.is_empty());
        assert_eq!(settings.curve_spec(), curve::BALANCED);
    }

    #[test]
    fn shipped_default_config_parses_to_defaults_without_issues() {
        let text = "\
config settings 'settings'
	option schema_version '2'
	option enabled '1'
	option mode 'auto'
	option curve 'balanced'
	option temp_source 'max'
	option manual_pwm '160'
	option interval '5'
	option hysteresis '2'
	option down_delay '30'
	option start_pwm '192'
	option start_boost_ms '700'
	option fail_pwm '255'
	option curve_silent '20:0,35:0,40:50,55:50,70:60,85:75,95:100'
	option curve_balanced '20:0,35:0,40:50,55:60,70:75,85:90,95:100'
	option curve_performance '20:0,30:60,45:70,60:82,75:92,85:100'
	option curve_custom '20:0,40:50,55:60,70:75,85:90,95:100'
";
        let settings = settings(text);
        assert!(settings.issues.is_empty(), "{:?}", settings.issues);
        assert_eq!(settings.schema_version, 2);
        assert_eq!(settings.curve_custom, curve::CUSTOM);
        assert_eq!(
            settings,
            Settings {
                schema_version: 2,
                ..Settings::default()
            },
            "the shipped default file must map onto the built-in defaults"
        );
    }

    #[test]
    fn legacy_off_mode_maps_to_kernel_protection() {
        let settings = settings("config settings 'settings'\n\toption mode 'off'\n");
        assert_eq!(settings.mode, Mode::Kernel);
        assert!(settings.is_kernel_only());
        assert_eq!(settings.issues.len(), 1);
    }

    #[test]
    fn unknown_mode_falls_back_to_auto() {
        let settings = settings("config settings 'settings'\n\toption mode 'turbo'\n");
        assert_eq!(settings.mode, Mode::Auto);
        assert_eq!(settings.issues[0].option, "mode");
    }

    #[test]
    fn out_of_range_values_are_clamped_with_a_reported_issue() {
        let settings = settings(
            "config settings 'settings'\n\
             \toption interval '900'\n\
             \toption down_delay '-5'\n\
             \toption fail_pwm '10'\n\
             \toption start_pwm '300'\n",
        );
        assert_eq!(settings.interval, 60);
        assert_eq!(settings.down_delay, 30, "non-numeric falls back");
        assert_eq!(settings.fail_pwm, 192, "failsafe must stay high");
        assert_eq!(settings.start_pwm, 255);
        let options: Vec<&str> = settings
            .issues
            .iter()
            .map(|issue| issue.option.as_str())
            .collect();
        assert_eq!(
            options,
            vec!["interval", "down_delay", "start_pwm", "fail_pwm"]
        );
    }

    #[test]
    fn kernel_only_detection_covers_disabled_and_kernel_mode() {
        assert!(settings("config settings 'settings'\n\toption enabled '0'\n").is_kernel_only());
        assert!(settings("config settings 'settings'\n\toption mode 'kernel'\n").is_kernel_only());
        assert!(!settings("config settings 'settings'\n\toption mode 'manual'\n").is_kernel_only());
    }

    #[test]
    fn curve_profile_selects_the_matching_text() {
        let base = "config settings 'settings'\n";
        for (value, expected) in [
            ("silent", curve::SILENT),
            ("balanced", curve::BALANCED),
            ("performance", curve::PERFORMANCE),
            ("custom", curve::CUSTOM),
        ] {
            let settings = settings(&format!("{base}\toption curve '{value}'\n"));
            assert_eq!(settings.curve_spec(), expected);
        }
    }

    #[test]
    fn invalid_custom_curve_is_reported_instead_of_defaulting_to_zero_speed() {
        let settings = settings(
            "config settings 'settings'\n\toption curve 'custom'\n\toption curve_custom '35:150'\n",
        );
        assert!(settings.curve_failure().is_some());
        // Control code turns this into `curve-failsafe`, never into 0 %.
        assert_eq!(settings.fail_pwm, 255);
    }

    #[test]
    fn policy_signature_changes_only_with_control_relevant_options() {
        let before = settings("config settings 'settings'\n\toption mode 'auto'\n");
        let after = settings("config settings 'settings'\n\toption mode 'manual'\n");
        assert_ne!(before.policy_signature(), after.policy_signature());
        let curve_edit = settings(
            "config settings 'settings'\n\toption curve 'custom'\n\toption curve_custom '20:0,50:60,90:100'\n",
        );
        assert_ne!(before.policy_signature(), curve_edit.policy_signature());
        // Editing a curve that is not selected must not force an apply cycle.
        let unused_edit =
            settings("config settings 'settings'\n\toption curve_custom '20:0,50:60,90:100'\n");
        assert_eq!(before.policy_signature(), unused_edit.policy_signature());
        // Purely cosmetic options must not force an apply cycle.
        let renamed = settings("config settings 'settings'\n\toption schema_version '3'\n");
        assert_eq!(before.policy_signature(), renamed.policy_signature());
    }
}
