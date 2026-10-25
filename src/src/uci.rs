//! Minimal UCI configuration file reader.
//!
//! Parsing `/etc/config/*` directly removes one `uci` process spawn per option
//! per control cycle, which the 2.x shell controller paid on every sample.
//! Committed UCI state lives in this file, so the values are identical; the
//! daemon re-reads it each cycle and therefore picks up edits without a
//! service restart.

use crate::paths::Paths;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub section_type: String,
    pub name: Option<String>,
    pub options: Vec<Option_>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Option_ {
    pub key: String,
    pub values: Vec<String>,
    pub is_list: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UciFile {
    pub sections: Vec<Section>,
    pub parse_warnings: Vec<String>,
}

fn unquote(raw: &str, warnings: &mut Vec<String>, line: usize) -> String {
    unquote_quiet(raw, false, warnings, line)
}

/// `warn=false` is used for the section type token, which UCI writes unquoted.
fn unquote_quiet(raw: &str, allow_bare: bool, warnings: &mut Vec<String>, line: usize) -> String {
    let text = raw.trim();
    let bytes: Vec<char> = text.chars().collect();
    if bytes.len() >= 2
        && ((bytes[0] == '\'' && bytes[bytes.len() - 1] == '\'')
            || (bytes[0] == '"' && bytes[bytes.len() - 1] == '"'))
    {
        let mut out = String::new();
        let mut index = 1;
        while index < bytes.len() - 1 {
            if bytes[index] == '\\' && index + 1 < bytes.len() - 1 {
                out.push(bytes[index + 1]);
                index += 2;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        return out;
    }
    if !allow_bare && !text.is_empty() && !matches!(text.as_bytes()[0], b'\'' | b'"') {
        warnings.push(format!(
            "line {line}: value {text:?} is not quoted; accepting it as-is"
        ));
    }
    text.to_string()
}

fn split_first_word(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let mut split = trimmed.splitn(2, char::is_whitespace);
    let head = split.next().unwrap_or_default();
    let rest = split.next().unwrap_or_default();
    Some((head, rest))
}

impl UciFile {
    pub fn parse(text: &str) -> Self {
        let mut file = UciFile::default();
        let mut current: Option<Section> = None;
        let mut pending: Option<(String, bool)> = None;
        let mut continuation = String::new();

        for (index, raw_line) in text.lines().enumerate() {
            let line = raw_line.trim_start();
            let line_number = index + 1;

            if !continuation.is_empty() {
                let chunk = raw_line.trim();
                if let Some(stripped) = chunk.strip_suffix('\\') {
                    continuation.push_str(stripped);
                    continue;
                }
                let joined = std::mem::take(&mut continuation) + chunk;
                if let (Some(section), Some((key, is_list))) = (current.as_mut(), pending.take()) {
                    push_option(
                        section,
                        key,
                        unquote(&joined, &mut file.parse_warnings, line_number),
                        is_list,
                    );
                }
                continue;
            }

            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((command, rest)) = split_first_word(line) else {
                continue;
            };
            match command {
                "config" => {
                    if let Some(section) = current.take() {
                        file.sections.push(section);
                    }
                    // `config 'type'` declares an anonymous section,
                    // `config type 'name'` declares a named one.
                    let tokens: Vec<&str> = rest.split_whitespace().collect();
                    if tokens.is_empty() {
                        file.parse_warnings
                            .push(format!("line {line_number}: empty config declaration"));
                        continue;
                    }
                    let type_name =
                        unquote_quiet(tokens[0], true, &mut file.parse_warnings, line_number);
                    let section_name = if tokens.len() > 1 {
                        Some(unquote(
                            tokens[tokens.len() - 1],
                            &mut file.parse_warnings,
                            line_number,
                        ))
                    } else {
                        None
                    };
                    current = Some(Section {
                        section_type: type_name,
                        name: section_name,
                        options: Vec::new(),
                    });
                }
                "option" | "list" => {
                    let is_list = command == "list";
                    let Some((key, value)) = split_first_word(rest) else {
                        file.parse_warnings
                            .push(format!("line {line_number}: {command} without a key"));
                        continue;
                    };
                    if value.trim_end().ends_with('\\') {
                        continuation.push_str(value.trim_end().trim_end_matches('\\'));
                        pending = Some((key.to_string(), is_list));
                        continue;
                    }
                    if let Some(section) = current.as_mut() {
                        push_option(
                            section,
                            key.to_string(),
                            unquote(value, &mut file.parse_warnings, line_number),
                            is_list,
                        );
                    } else {
                        file.parse_warnings.push(format!(
                            "line {line_number}: option {key} outside a section"
                        ));
                    }
                }
                other => {
                    file.parse_warnings
                        .push(format!("line {line_number}: unknown directive {other:?}"));
                }
            }
        }

        if let Some(section) = current {
            file.sections.push(section);
        }
        file
    }

    pub fn read(paths: &Paths) -> std::io::Result<UciFile> {
        UciFile::read_path(&paths.uci_config)
    }

    pub fn read_path(path: &Path) -> std::io::Result<UciFile> {
        let text = std::fs::read_to_string(path)?;
        Ok(UciFile::parse(&text))
    }

    pub fn section(&self, section_type: &str, name: &str) -> Option<&Section> {
        self.sections
            .iter()
            .find(|s| s.section_type == section_type && s.name.as_deref() == Some(name))
    }

    /// Value of `settings.<option>`, the layout this package always uses.
    pub fn setting(&self, option: &str) -> Option<&str> {
        let section = self
            .section("settings", "settings")
            .or_else(|| self.section("settings", "settings_"))
            .or_else(|| {
                self.sections
                    .iter()
                    .find(|s| s.section_type == "settings" && s.name.is_some())
            })?;
        section
            .options
            .iter()
            .find(|o| o.key == option)
            .and_then(|o| o.values.first())
            .map(String::as_str)
    }
}

fn push_option(section: &mut Section, key: String, value: String, is_list: bool) {
    match section.options.iter_mut().find(|o| o.key == key) {
        Some(existing) if is_list || existing.is_list => existing.values.push(value),
        Some(existing) => existing.values = vec![value],
        None => section.options.push(Option_ {
            key,
            values: vec![value],
            is_list,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# H5000M fan control
config settings 'settings'
	option schema_version '2'
	option enabled '1'
	option mode 'auto'
	option curve_custom '20:0,40:50,55:60,70:75,85:90,95:100'
	list allow 'ra0'
	list allow 'rai0'

config device 'lan'
	option name 'br-lan'
";

    #[test]
    fn parses_named_sections_and_options() {
        let file = UciFile::parse(SAMPLE);
        assert_eq!(file.setting("mode"), Some("auto"));
        assert_eq!(file.setting("schema_version"), Some("2"));
        assert_eq!(
            file.setting("curve_custom"),
            Some("20:0,40:50,55:60,70:75,85:90,95:100")
        );
        let settings = file.section("settings", "settings").unwrap();
        let allow = settings.options.iter().find(|o| o.key == "allow").unwrap();
        assert_eq!(allow.values, vec!["ra0".to_string(), "rai0".to_string()]);
        assert!(allow.is_list);
        assert_eq!(
            file.section("device", "lan").unwrap().options[0].values[0],
            "br-lan"
        );
        assert!(file.parse_warnings.is_empty());
    }

    #[test]
    fn anonymous_section_and_quotes_are_supported() {
        let file = UciFile::parse("config settings\n\toption mode \"manual\"\n");
        assert_eq!(
            file.sections[0].name, None,
            "config with a single token is anonymous"
        );
        assert_eq!(file.sections[0].options[0].values[0], "manual");
    }

    #[test]
    fn escaped_quotes_survive() {
        let file = UciFile::parse("config settings 'settings'\n\toption note 'a\\'b'\n");
        assert_eq!(file.setting("note"), Some("a'b"));
    }

    #[test]
    fn repeated_option_keeps_the_last_value() {
        let file = UciFile::parse(
            "config settings 'settings'\n\toption mode 'auto'\n\toption mode 'manual'\n",
        );
        assert_eq!(file.setting("mode"), Some("manual"));
    }

    #[test]
    fn unknown_directive_is_a_warning_not_a_failure() {
        let file =
            UciFile::parse("config settings 'settings'\n\thelper 'x'\n\toption mode 'auto'\n");
        assert_eq!(file.setting("mode"), Some("auto"));
        assert_eq!(file.parse_warnings.len(), 1);
    }
}
