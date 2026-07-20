//! Минимальная сериализация JSON без serde.
//!
//! Нам нужно только *писать* JSON (ответы API) и разбирать простейшие
//! `application/x-www-form-urlencoded` тела запросов, поэтому полноценный
//! парсер JSON не требуется.

use std::fmt::Write as _;

/// Значение JSON, которое умеем выводить.
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Int(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    /// `Some(v)` -> значение, `None` -> null.
    pub fn opt_int(v: Option<i64>) -> Json {
        match v {
            Some(v) => Json::Int(v),
            None => Json::Null,
        }
    }

    pub fn opt_num(v: Option<f64>) -> Json {
        match v {
            Some(v) => Json::Num(v),
            None => Json::Null,
        }
    }

    pub fn opt_str(v: Option<String>) -> Json {
        match v {
            Some(v) => Json::Str(v),
            None => Json::Null,
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Int(i) => {
                let _ = write!(out, "{}", i);
            }
            Json::Num(f) => {
                if f.is_finite() {
                    let _ = write!(out, "{}", f);
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(s) => escape_into(s, out),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    escape_into(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// `Display` даёт бесплатный `.to_string()` и работу в `format!`.
impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

/// Экранирование строки по RFC 8259, включая управляющие символы.
fn escape_into(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            // Экранируем и U+2028/2029: иначе ломается инлайн-JSON в <script>.
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Разбор `application/x-www-form-urlencoded` (и строки запроса).
pub fn parse_form(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.push((url_decode(k), url_decode(v)));
    }
    out
}

pub fn form_get<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    form.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_scalars() {
        assert_eq!(Json::Null.to_string(), "null");
        assert_eq!(Json::Bool(true).to_string(), "true");
        assert_eq!(Json::Int(-42).to_string(), "-42");
        assert_eq!(Json::str("hi").to_string(), "\"hi\"");
    }

    #[test]
    fn escapes_strings() {
        assert_eq!(
            Json::str("a\"b\\c\nd\u{1}").to_string(),
            "\"a\\\"b\\\\c\\nd\\u0001\""
        );
    }

    #[test]
    fn writes_nested() {
        let v = Json::obj(vec![
            ("a", Json::Int(1)),
            ("b", Json::Arr(vec![Json::Int(2), Json::Null])),
        ]);
        assert_eq!(v.to_string(), r#"{"a":1,"b":[2,null]}"#);
    }

    #[test]
    fn non_finite_numbers_become_null() {
        assert_eq!(Json::Num(f64::NAN).to_string(), "null");
        assert_eq!(Json::Num(f64::INFINITY).to_string(), "null");
    }

    #[test]
    fn parses_form() {
        let f = parse_form("earfcn=2850&pci=57&cmd=at%5Eefs%3D%22x%22&empty=");
        assert_eq!(form_get(&f, "earfcn"), Some("2850"));
        assert_eq!(form_get(&f, "pci"), Some("57"));
        assert_eq!(form_get(&f, "cmd"), Some("at^efs=\"x\""));
        assert_eq!(form_get(&f, "empty"), Some(""));
        assert_eq!(form_get(&f, "nope"), None);
    }

    #[test]
    fn form_decodes_plus_and_bad_escapes() {
        let f = parse_form("a=x+y&b=100%&c=%zz");
        assert_eq!(form_get(&f, "a"), Some("x y"));
        assert_eq!(form_get(&f, "b"), Some("100%"));
        assert_eq!(form_get(&f, "c"), Some("%zz"));
    }
}
