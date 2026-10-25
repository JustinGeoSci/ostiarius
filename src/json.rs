//  src/json.rs

//! The `--json` output is written by this minimal JSON writer. It supports
//! objects, arrays, strings, unsigned integers and booleans, and it keeps keys
//! in the order they are given so the output is stable from run to run.

use std::fmt::Write as _;

// ===========================================================================
// Values
// ===========================================================================

pub(crate) enum Value {
    Bool(bool),
    Num(u64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(&'static str, Value)>),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Self {
        Self::Str(s.into())
    }

    pub fn strs<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Arr(items.into_iter().map(|s| Self::Str(s.into())).collect())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Num(n) => {
                let _ = write!(out, "{n}");
            }
            Self::Str(s) => write_str(s, out),
            Self::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::Value;

    #[test]
    fn escapes_quotes_and_control_chars() {
        assert_eq!(
            Value::str("a\"b\\c\nd\u{1}").render(),
            r#""a\"b\\c\nd\u0001""#
        );
    }

    #[test]
    fn nested_objects_and_arrays() {
        let v = Value::Obj(vec![
            ("ok", Value::Bool(false)),
            ("n", Value::Num(3)),
            ("names", Value::strs(["a", "b"])),
            ("inner", Value::Obj(vec![("x", Value::Arr(vec![]))])),
        ]);
        assert_eq!(
            v.render(),
            r#"{"ok":false,"n":3,"names":["a","b"],"inner":{"x":[]}}"#
        );
    }
}
