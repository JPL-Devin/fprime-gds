//! JSON text as Python `json.loads` reads it, for the arrays and structs given as command arguments: an
//! integer literal of any size is an integer, a float literal beyond a double is an infinity, and `NaN`,
//! `Infinity`, `-Infinity` are numbers. The values have a type of their own, so no text a user writes can
//! be taken for anything but what Python takes it for.

/// A value of JSON text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PyJson {
    Null,
    Bool(bool),
    /// The literal as written: `-` and decimal digits.
    Int(String),
    Float(f64),
    Str(String),
    List(Vec<PyJson>),
    /// In the order written; of members with one name, Python keeps the last.
    Dict(Vec<(String, PyJson)>),
}

impl PyJson {
    /// The name of the Python class of the value.
    pub(crate) fn class(&self) -> &'static str {
        match self {
            PyJson::Null => "NoneType",
            PyJson::Bool(_) => "bool",
            PyJson::Int(_) => "int",
            PyJson::Float(_) => "float",
            PyJson::Str(_) => "str",
            PyJson::List(_) => "list",
            PyJson::Dict(_) => "dict",
        }
    }

    /// The member of a dict, as Python looks it up.
    #[cfg(test)]
    pub(crate) fn get(&self, name: &str) -> Option<&PyJson> {
        match self {
            PyJson::Dict(members) => members.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }
}

/// The value of `text`, or `None` where `json.loads` raises.
pub(crate) fn loads(text: &str) -> Option<PyJson> {
    let mut parser = Parser { text, at: 0 };
    let value = parser.value()?;
    parser.space();
    (parser.at == text.len()).then_some(value)
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn word(&mut self, word: &str) -> bool {
        let found = self.text[self.at..].starts_with(word);
        if found {
            self.at += word.len();
        }
        found
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn value(&mut self) -> Option<PyJson> {
        self.space();
        match self.peek()? {
            b'{' => {
                self.at += 1;
                let mut members = Vec::new();
                self.space();
                if self.word("}") {
                    return Some(PyJson::Dict(members));
                }
                loop {
                    self.space();
                    let name = self.string()?;
                    self.space();
                    if !self.word(":") {
                        return None;
                    }
                    members.push((name, self.value()?));
                    self.space();
                    if self.word("}") {
                        return Some(PyJson::Dict(members));
                    }
                    if !self.word(",") {
                        return None;
                    }
                }
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.word("]") {
                    return Some(PyJson::List(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    if self.word("]") {
                        return Some(PyJson::List(items));
                    }
                    if !self.word(",") {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(PyJson::Str),
            _ if self.word("null") => Some(PyJson::Null),
            _ if self.word("true") => Some(PyJson::Bool(true)),
            _ if self.word("false") => Some(PyJson::Bool(false)),
            _ if self.word("NaN") => Some(PyJson::Float(f64::NAN)),
            _ if self.word("Infinity") => Some(PyJson::Float(f64::INFINITY)),
            _ if self.word("-Infinity") => Some(PyJson::Float(f64::NEG_INFINITY)),
            _ => self.number(),
        }
    }

    /// `-?(0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?`, as the number pattern of the Python decoder. A fraction or an
    /// exponent that is not complete is left unread, so the text is refused for what follows the integer.
    fn number(&mut self) -> Option<PyJson> {
        let start = self.at;
        self.word("-");
        match self.peek()? {
            b'0' => self.at += 1,
            b'1'..=b'9' => {
                self.digits();
            }
            _ => return None,
        }
        let integer = self.at;
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                self.at = integer;
            }
        }
        let fraction = self.at;
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'-' | b'+')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                self.at = fraction;
            }
        }
        let literal = &self.text[start..self.at];
        if self.at == integer {
            Some(PyJson::Int(literal.to_string()))
        } else {
            // As Python `float(literal)`: the nearest double, an infinity beyond the largest.
            literal.parse().ok().map(PyJson::Float)
        }
    }

    /// A string, read by `serde_json`, which takes the escapes of the Python decoder and refuses, as it does,
    /// a control character that is not escaped.
    fn string(&mut self) -> Option<String> {
        let bytes = self.text.as_bytes();
        let start = self.at;
        if self.peek()? != b'"' {
            return None;
        }
        let mut end = start + 1;
        while *bytes.get(end)? != b'"' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        self.at = end + 1;
        serde_json::from_str(self.text.get(start..self.at)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_read_as_python_reads_them() {
        let int = |text: &str| Some(PyJson::Int(text.to_string()));
        assert_eq!(loads("18446744073709551616"), int("18446744073709551616"));
        assert_eq!(loads(" -0 "), int("-0"));
        assert_eq!(loads("1e400"), Some(PyJson::Float(f64::INFINITY)));
        assert_eq!(loads("-1e400"), Some(PyJson::Float(f64::NEG_INFINITY)));
        assert_eq!(loads("-Infinity"), Some(PyJson::Float(f64::NEG_INFINITY)));
        assert!(matches!(loads("NaN"), Some(PyJson::Float(v)) if v.is_nan()));
        assert_eq!(loads("1.0"), Some(PyJson::Float(1.0)));
        for refused in ["05", "+5", "5.", ".5", "0x10", "1_0", "- 5", "+Infinity", "-NaN", "nan", "1e", "{", "[1,]", ""]
        {
            assert_eq!(loads(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_marker_cannot_be_written() {
        let value = loads(r#"{"a": {"\u0000int": "5"}, "b": [1, "x"], "a": null}"#).unwrap();
        assert_eq!(value.get("b"), Some(&PyJson::List(vec![PyJson::Int("1".into()), PyJson::Str("x".into())])));
        assert_eq!(value.get("a"), Some(&PyJson::Null), "the last of two members with one name");
        let first = match &value {
            PyJson::Dict(members) => &members[0].1,
            _ => unreachable!(),
        };
        assert_eq!(first.class(), "dict");
    }
}
