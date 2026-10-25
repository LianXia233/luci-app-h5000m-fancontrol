//! UCI configuration access.
//!
//! The shell controller issued one `uci get` per option on every reload. The
//! Rust controller keeps those semantics (including uncommitted `/tmp/.uci-*`
//! deltas) but reads the whole config with a single `uci show` call, cached by
//! the modification time of the config file and delta file so the daemon loop
//! no longer spawns ~18 processes per interval.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;

pub const CONFIG_NAME: &str = "h5000m_fancontrol";
const SECTION: &str = "settings";

/// Anything able to provide the effective `settings` section values.
pub trait ConfigSource {
    fn settings(&mut self) -> HashMap<String, String>;
}

/// Production source backed by the `uci` command.
pub struct UciShowSource {
    config_path: PathBuf,
    delta_path: PathBuf,
    cache: Option<HashMap<String, String>>,
    cached_signature: Option<(Option<SystemTime>, Option<SystemTime>)>,
}

impl UciShowSource {
    pub fn new() -> Self {
        UciShowSource {
            config_path: PathBuf::from("/etc/config").join(CONFIG_NAME),
            delta_path: PathBuf::from("/tmp/.uci-").join(CONFIG_NAME),
            cache: None,
            cached_signature: None,
        }
    }

    fn signature(&self) -> (Option<SystemTime>, Option<SystemTime>) {
        (
            std::fs::metadata(&self.config_path)
                .and_then(|m| m.modified())
                .ok(),
            std::fs::metadata(&self.delta_path)
                .and_then(|m| m.modified())
                .ok(),
        )
    }
}

impl Default for UciShowSource {
    fn default() -> Self {
        UciShowSource::new()
    }
}

impl ConfigSource for UciShowSource {
    fn settings(&mut self) -> HashMap<String, String> {
        let signature = self.signature();
        if self.cache.is_some() && self.cached_signature == Some(signature) {
            return self.cache.clone().unwrap_or_default();
        }
        let parsed = parse_uci_show(
            &Command::new("uci")
                .arg("-q")
                .arg("show")
                .arg(CONFIG_NAME)
                .output(),
        );
        self.cache = Some(parsed.clone());
        self.cached_signature = Some(signature);
        parsed
    }
}

/// Parses `uci show h5000m_fancontrol` output of the form
/// `h5000m_fancontrol.settings.option='value'`.
pub fn parse_uci_show(
    output: &Result<std::process::Output, std::io::Error>,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(out) = output else { return map };
    if !out.status.success() {
        return map;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let prefix = format!("{CONFIG_NAME}.{SECTION}.");
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let Some(eq) = rest.find('=') else { continue };
        let key = &rest[..eq];
        if key.is_empty() || key.contains('.') {
            continue;
        }
        map.insert(key.to_string(), unquote(&rest[eq + 1..]));
    }
    map
}

/// Removes the surrounding single quotes `uci show` adds and undoes the
/// `'\''` escaping it applies to embedded quotes.
pub fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        trimmed[1..trimmed.len() - 1].replace("'\\''", "'")
    } else {
        trimmed.to_string()
    }
}

/// In-process source for tests and diagnostics.
pub struct MapSource(pub HashMap<String, String>);

impl ConfigSource for MapSource {
    fn settings(&mut self) -> HashMap<String, String> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn fake_output(stdout: &str) -> Result<Output, std::io::Error> {
        Ok(Output {
            status: ExitStatus::from_raw(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    #[test]
    fn parses_settings_options_and_ignores_other_sections() {
        let out = fake_output(
            "h5000m_fancontrol=settings\n\
             h5000m_fancontrol.settings=settings\n\
             h5000m_fancontrol.settings.mode='auto'\n\
             h5000m_fancontrol.settings.curve_custom='20:0,40:50'\n\
             h5000m_fancontrol.other.foo='1'\n",
        );
        let map = parse_uci_show(&out);
        assert_eq!(map.get("mode").map(String::as_str), Some("auto"));
        assert_eq!(
            map.get("curve_custom").map(String::as_str),
            Some("20:0,40:50")
        );
        assert!(!map.contains_key("foo"));
    }

    #[test]
    fn unquotes_escaped_single_quotes() {
        // uci show renders it's as 'it'\''s'
        assert_eq!(unquote("'it'\\''s'"), "it's");
        assert_eq!(unquote("plain"), "plain");
    }
}
