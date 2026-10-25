//! Minimal JSON writer used for the structured status interface.
//!
//! The status schema only contains strings, integers, booleans and null, so a
//! small purpose-built writer keeps the controller free of serialization
//! dependencies while guaranteeing stable field names and UTF-8 output.

pub fn escape_into(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn string(value: &str) -> String {
    let mut out = String::new();
    escape_into(&mut out, value);
    out
}

/// Builder for flat and one-level-nested JSON objects.
#[derive(Default)]
pub struct ObjectWriter {
    parts: Vec<String>,
}

impl ObjectWriter {
    pub fn new() -> Self {
        ObjectWriter::default()
    }

    fn push_raw(&mut self, key: &str, raw: String) {
        self.parts.push(format!("{}:{}", string(key), raw));
    }

    pub fn str(&mut self, key: &str, value: &str) -> &mut Self {
        self.push_raw(key, string(value));
        self
    }

    pub fn opt_str(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        match value {
            Some(v) => self.push_raw(key, string(v)),
            None => self.push_raw(key, "null".to_string()),
        }
        self
    }

    pub fn int(&mut self, key: &str, value: i64) -> &mut Self {
        self.push_raw(key, value.to_string());
        self
    }

    pub fn opt_int(&mut self, key: &str, value: Option<i64>) -> &mut Self {
        match value {
            Some(v) => self.push_raw(key, v.to_string()),
            None => self.push_raw(key, "null".to_string()),
        }
        self
    }

    pub fn bool(&mut self, key: &str, value: bool) -> &mut Self {
        self.push_raw(key, value.to_string());
        self
    }

    pub fn raw(&mut self, key: &str, value: String) -> &mut Self {
        self.push_raw(key, value);
        self
    }

    pub fn finish(&self) -> String {
        format!("{{{}}}", self.parts.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_special_characters() {
        assert_eq!(string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn writes_nested_objects_and_nulls() {
        let mut inner = ObjectWriter::new();
        inner.int("temp", 42).opt_int("rpm", None).bool("ok", true);
        let mut outer = ObjectWriter::new();
        outer.str("mode", "auto").raw("sensors", inner.finish());
        let json = outer.finish();
        assert_eq!(
            json,
            "{\"mode\":\"auto\",\"sensors\":{\"temp\":42,\"rpm\":null,\"ok\":true}}"
        );
    }
}
