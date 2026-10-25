//! A tiny JSON value tree and writer.
//!
//! The status contract needs correct escaping and a stable key order, not a
//! full serde stack, so this stays in-tree with the crate.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn str(value: impl Into<String>) -> Value {
        Value::Str(value.into())
    }

    pub fn opt_str(value: Option<impl Into<String>>) -> Value {
        match value {
            Some(inner) => Value::Str(inner.into()),
            None => Value::Null,
        }
    }

    pub fn opt_int(value: Option<impl Into<i64>>) -> Value {
        match value {
            Some(inner) => Value::Int(inner.into()),
            None => Value::Null,
        }
    }

    pub fn object(pairs: impl IntoIterator<Item = (impl Into<String>, Value)>) -> Value {
        Value::Object(
            pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    pub fn array(values: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(values.into_iter().collect())
    }

    pub fn write_to(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Value::Str(text) => write_string(text, out),
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write_to(out);
                }
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (index, (key, value)) in pairs.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    value.write_to(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(1024);
        self.write_to(&mut out);
        out
    }

    /// Dotted paths of every key the writer can emit, arrays collapsed to `[]`.
    ///
    /// `h5000m-fancontrol schema` prints this so CI can prove the frontend and
    /// the backend agree on the field names.
    pub fn paths(&self) -> Vec<String> {
        let mut out = Vec::new();
        collect(self, String::new(), &mut out);
        out.sort();
        out.dedup();
        out
    }
}

fn collect(value: &Value, prefix: String, out: &mut Vec<String>) {
    match value {
        Value::Object(pairs) => {
            for (key, child) in pairs {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.push(path.clone());
                collect(child, path, out);
            }
        }
        Value::Array(items) => {
            let path = format!("{prefix}[]");
            for item in items {
                collect(item, path.clone(), out);
            }
        }
        _ => {}
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    write_string(text, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_backslashes_and_control_bytes() {
        assert_eq!(escape("a\"b"), "\"a\\\"b\"");
        assert_eq!(escape("a\\b"), "\"a\\\\b\"");
        assert_eq!(escape("line\nnext"), "\"line\\nnext\"");
        assert_eq!(escape("bell\u{7}"), "\"bell\\u0007\"");
    }

    #[test]
    fn non_ascii_is_kept_literally() {
        // LuCI parses the response as UTF-8; escaping to \uXXXX only bloats it.
        let value = Value::str("风扇控制");
        assert_eq!(value.to_json(), "\"风扇控制\"");
    }

    #[test]
    fn key_order_is_the_insertion_order() {
        let value = Value::object([
            ("second", Value::Int(2)),
            ("first", Value::Int(1)),
            ("missing", Value::Null),
        ]);
        assert_eq!(
            value.to_json(),
            "{\"second\":2,\"first\":1,\"missing\":null}"
        );
    }

    #[test]
    fn option_fields_render_as_null_not_zero() {
        let value = Value::object([
            ("cpu_temp", Value::opt_int(None::<i32>)),
            ("phy_temp", Value::opt_int(Some(52i32))),
            ("label", Value::opt_str(None::<String>)),
        ]);
        assert_eq!(
            value.to_json(),
            "{\"cpu_temp\":null,\"phy_temp\":52,\"label\":null}"
        );
    }

    #[test]
    fn paths_flatten_arrays_and_include_parents() {
        let value = Value::object([
            (
                "sensors",
                Value::array([Value::object([("value_c", Value::Int(1))])]),
            ),
            ("controller", Value::object([("pid", Value::Int(9))])),
        ]);
        assert_eq!(
            value.paths(),
            vec![
                "controller".to_string(),
                "controller.pid".to_string(),
                "sensors".to_string(),
                "sensors[].value_c".to_string(),
            ]
        );
    }

    #[test]
    fn empty_array_produces_no_child_paths() {
        let value = Value::object([("events", Value::array(Vec::new()))]);
        assert_eq!(value.paths(), vec!["events".to_string()]);
        assert_eq!(value.to_json(), "{\"events\":[]}");
    }
}
