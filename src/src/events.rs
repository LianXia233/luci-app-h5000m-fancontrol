//! Bounded event journal and log emission.
//!
//! The daemon writes a small ring of JSON lines under `/var/run` so the
//! diagnostics tab can show what happened without granting LuCI access to
//! `logread`, and without a second process per message.

use crate::json::Value;
use crate::paths::Paths;
use std::collections::HashMap;

pub const MAX_EVENTS: usize = 120;
pub const MAX_BYTES: u64 = 48 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// `LOG_DAEMON` facility priorities.
    pub fn syslog_priority(self) -> u8 {
        3 * 8
            + match self {
                Level::Info => 6,
                Level::Warn => 4,
                Level::Error => 3,
            }
    }

    pub fn parse(raw: &str) -> Level {
        match raw {
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub ts: u64,
    pub level: Level,
    pub code: String,
    pub message: String,
}

impl Event {
    pub fn new(level: Level, code: impl Into<String>, message: impl Into<String>, ts: u64) -> Self {
        Event {
            ts,
            level,
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn to_value(&self) -> Value {
        Value::object([
            ("ts", Value::Int(self.ts as i64)),
            ("level", Value::str(self.level.as_str())),
            ("code", Value::str(self.code.clone())),
            ("message", Value::str(self.message.clone())),
        ])
    }

    fn to_line(&self) -> String {
        format!("{}\n", self.to_value().to_json())
    }

    fn from_line(line: &str) -> Option<Event> {
        // Deliberately small: the journal is written by this same code, so only
        // the four members it emits are looked for.
        let ts = scan_number(line, "ts")?;
        let level = scan_string(line, "level").map(|value| Level::parse(&value))?;
        let code = scan_string(line, "code")?;
        let message = scan_string(line, "message").unwrap_or_default();
        Some(Event {
            ts: ts as u64,
            level,
            code,
            message,
        })
    }
}

fn scan_number(haystack: &str, key: &str) -> Option<i64> {
    let marker = format!("\"{key}\":");
    let at = haystack.find(&marker)? + marker.len();
    let rest = &haystack[at..];
    let digits: String = rest
        .chars()
        .skip_while(|c| *c == '-')
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<i64>().ok()
}

fn scan_string(haystack: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\":\"");
    let at = haystack.find(&marker)? + marker.len();
    let rest = &haystack[at..];
    let mut out = String::new();
    let mut escaped = false;
    for character in rest.chars() {
        if escaped {
            out.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    Some(out)
}

/// Append one event, rotating the tail so the file cannot grow without bound.
pub fn append(paths: &Paths, event: &Event) -> crate::Result<()> {
    let path = paths.event_log();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut existing = std::fs::read(&path).unwrap_or_default();
    if !existing.is_empty() && !existing.ends_with(b"\n") {
        existing.push(b'\n');
    }
    let mut text = String::from_utf8_lossy(&existing).to_string();
    text.push_str(&event.to_line());
    let bytes = text.as_bytes();
    if bytes.len() as u64 > MAX_BYTES {
        // Drop from the front, on a line boundary, until we are back under the
        // limit with room to spare.
        let keep_from = bytes.len() - (MAX_BYTES as usize / 2);
        let mut start = keep_from;
        while start < bytes.len() && bytes[start] != b'\n' {
            start += 1;
        }
        if start < bytes.len() {
            start += 1;
        }
        text = String::from_utf8_lossy(&bytes[start..]).to_string();
    }
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() > MAX_EVENTS {
        lines.drain(0..lines.len() - MAX_EVENTS);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    std::fs::write(&path, out).map_err(|err| crate::Error::io(path.display().to_string(), &err))
}

/// Most recent events, newest first.
pub fn read_recent(paths: &Paths, limit: usize) -> Vec<Event> {
    let Ok(text) = std::fs::read_to_string(paths.event_log()) else {
        return Vec::new();
    };
    let mut events: Vec<Event> = text.lines().filter_map(Event::from_line).collect();
    events.reverse();
    events.truncate(limit);
    events
}

pub fn to_values(events: &[Event]) -> Vec<Value> {
    events.iter().map(Event::to_value).collect()
}

/// Suppress repeated identical messages, keeping the count of what was hidden.
#[derive(Default)]
pub struct Throttle {
    seen: HashMap<String, (u64, u64)>,
    window: u64,
}

impl Throttle {
    pub fn new(window: u64) -> Self {
        Throttle {
            seen: HashMap::new(),
            window,
        }
    }

    pub fn allow(&mut self, key: &str, now: u64) -> bool {
        match self.seen.get(key) {
            Some((last, _)) if now.saturating_sub(*last) < self.window => false,
            _ => {
                self.seen.insert(key.to_string(), (now, 0));
                true
            }
        }
    }

    /// Forget keys that have not repeated for a long time, so a long running
    /// daemon cannot grow its memory with one-shot warnings.
    pub fn prune(&mut self, now: u64) {
        let window = self.window.saturating_mul(4).max(1);
        self.seen
            .retain(|_, (last, _)| now.saturating_sub(*last) <= window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform;

    fn temp_paths(label: &str) -> Paths {
        let root = std::env::temp_dir().join(format!(
            "h5fan-events-{label}-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Paths::sandbox(root)
    }

    #[test]
    fn events_round_trip_through_the_journal() {
        let paths = temp_paths("roundtrip");
        let event = Event::new(Level::Warn, "sensor-failsafe", "CPU reading lost", 1234);
        append(&paths, &event).unwrap();
        let read = read_recent(&paths, 10);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0], event);
    }

    #[test]
    fn messages_with_quotes_and_newlines_stay_one_line() {
        let paths = temp_paths("escaping");
        let event = Event::new(
            Level::Error,
            "pwm-write-failed",
            "cannot write \"value\"\nsecond line",
            1,
        );
        append(&paths, &event).unwrap();
        let raw = std::fs::read_to_string(paths.event_log()).unwrap();
        assert_eq!(raw.lines().count(), 1);
        let read = read_recent(&paths, 5);
        assert_eq!(read[0].message, "cannot write \"value\"\nsecond line");
    }

    #[test]
    fn journal_is_bounded_in_entries() {
        let paths = temp_paths("bound");
        for index in 0..(MAX_EVENTS + 40) {
            append(
                &paths,
                &Event::new(Level::Info, "cycle", format!("event {index}"), index as u64),
            )
            .unwrap();
        }
        let raw = std::fs::read_to_string(paths.event_log()).unwrap();
        assert_eq!(raw.lines().count(), MAX_EVENTS);
        let recent = read_recent(&paths, 3);
        assert_eq!(recent[0].message, format!("event {}", MAX_EVENTS + 39));
        assert_eq!(recent[0].ts, (MAX_EVENTS + 39) as u64);
    }

    #[test]
    fn journal_is_bounded_in_bytes() {
        let paths = temp_paths("bytes");
        let big = "x".repeat(2000);
        for index in 0..60 {
            append(&paths, &Event::new(Level::Warn, "flood", &big, index)).unwrap();
        }
        let size = std::fs::metadata(paths.event_log()).unwrap().len();
        assert!(size <= MAX_BYTES, "{size} bytes must stay bounded");
        assert!(
            !read_recent(&paths, 1).is_empty(),
            "the newest event survives"
        );
    }

    #[test]
    fn truncated_last_line_is_ignored_not_fatal() {
        let paths = temp_paths("partial");
        append(&paths, &Event::new(Level::Info, "a", "first", 1)).unwrap();
        let mut raw = std::fs::read_to_string(paths.event_log()).unwrap();
        raw.push_str("{\"ts\":2,\"level\":\"inf");
        std::fs::write(paths.event_log(), raw).unwrap();
        let read = read_recent(&paths, 10);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].code, "a");
    }

    #[test]
    fn throttle_suppresses_repeats_inside_its_window() {
        let mut throttle = Throttle::new(60);
        assert!(throttle.allow("sensor-lost", 100));
        assert!(!throttle.allow("sensor-lost", 120));
        assert!(throttle.allow("sensor-lost", 161));
        assert!(throttle.allow("other", 161));
        throttle.prune(10_000);
        assert!(throttle.seen.is_empty(), "stale keys are dropped");
    }

    #[test]
    fn values_use_the_json_writer() {
        let value = Event::new(Level::Info, "ok", "温度 ok", 7).to_value();
        assert!(value.to_json().contains("温度 ok"));
        assert_eq!(crate::json::escape("a\"b"), "\"a\\\"b\"");
    }
}
