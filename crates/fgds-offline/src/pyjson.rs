//! The message Python's `json.loads` gives for text that is not JSON, so that a rejected
//! argument reads the same as with the Python tools. Only the error is found here; values
//! are parsed by `serde_json`.

struct Fault {
    message: String,
    at: usize,
}

fn fault<T>(message: impl Into<String>, at: usize) -> Result<T, Fault> {
    Err(Fault { message: message.into(), at })
}

fn skip(s: &[char], mut at: usize) -> usize {
    while matches!(s.get(at), Some(' ' | '\t' | '\n' | '\r')) {
        at += 1;
    }
    at
}

fn starts(s: &[char], at: usize, word: &str) -> bool {
    word.chars().enumerate().all(|(i, c)| s.get(at + i) == Some(&c))
}

/// The end of the number at `at`, if there is one.
fn number(s: &[char], at: usize) -> Option<usize> {
    let digits = |mut i: usize| {
        while s.get(i).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
        i
    };
    let mut end = at + usize::from(s.get(at) == Some(&'-'));
    match s.get(end) {
        Some('0') => end += 1,
        Some(c) if c.is_ascii_digit() => end = digits(end),
        _ => return None,
    }
    if s.get(end) == Some(&'.') && digits(end + 1) > end + 1 {
        end = digits(end + 1);
    }
    if matches!(s.get(end), Some('e' | 'E')) {
        let exponent = end + 1 + usize::from(matches!(s.get(end + 1), Some('+' | '-')));
        if digits(exponent) > exponent {
            end = digits(exponent);
        }
    }
    Some(end)
}

/// The end of the string whose opening quote is at `at - 1`.
fn string(s: &[char], at: usize) -> Result<usize, Fault> {
    let begin = at - 1;
    let mut i = at;
    loop {
        match s.get(i) {
            None => return fault("Unterminated string starting at", begin),
            Some('"') => return Ok(i + 1),
            Some(c) if (*c as u32) < 0x20 => return fault("Invalid control character at", i),
            Some('\\') => match s.get(i + 1) {
                None => return fault("Unterminated string starting at", begin),
                Some('u') => {
                    let hex = (2..6).all(|k| s.get(i + k).is_some_and(char::is_ascii_hexdigit));
                    if !hex {
                        return fault("Invalid \\uXXXX escape", i + 1);
                    }
                    i += 6;
                }
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => i += 2,
                Some(_) => return fault("Invalid \\escape", i),
            },
            Some(_) => i += 1,
        }
    }
}

const DEPTH: usize = 512;

/// The end of the value at `at`.
fn value(s: &[char], at: usize, depth: usize) -> Result<usize, Fault> {
    let nothing = || fault("Expecting value", at);
    if depth > DEPTH {
        return nothing();
    }
    match s.get(at) {
        Some('"') => string(s, at + 1),
        Some('{') => {
            let mut end = skip(s, at + 1);
            if s.get(end) == Some(&'}') {
                return Ok(end + 1);
            }
            loop {
                if s.get(end) != Some(&'"') {
                    return fault("Expecting property name enclosed in double quotes", end);
                }
                end = skip(s, string(s, end + 1)?);
                if s.get(end) != Some(&':') {
                    return fault("Expecting ':' delimiter", end);
                }
                end = skip(s, value(s, skip(s, end + 1), depth + 1)?);
                match s.get(end) {
                    Some('}') => return Ok(end + 1),
                    Some(',') => end = skip(s, end + 1),
                    _ => return fault("Expecting ',' delimiter", end),
                }
            }
        }
        Some('[') => {
            let mut end = skip(s, at + 1);
            if s.get(end) == Some(&']') {
                return Ok(end + 1);
            }
            loop {
                end = skip(s, value(s, end, depth + 1)?);
                match s.get(end) {
                    Some(']') => return Ok(end + 1),
                    Some(',') => end = skip(s, end + 1),
                    _ => return fault("Expecting ',' delimiter", end),
                }
            }
        }
        Some(_) => {
            let words = ["null", "true", "false", "NaN", "Infinity", "-Infinity"];
            match (number(s, at), words.iter().find(|w| starts(s, at, w))) {
                (Some(end), _) => Ok(end),
                (None, Some(word)) => Ok(at + word.len()),
                _ => nothing(),
            }
        }
        None => nothing(),
    }
}

/// The message of `json.loads(text)` when it raises, `None` when it accepts the text.
pub fn error(text: &str) -> Option<String> {
    let s: Vec<char> = text.chars().collect();
    let found = value(&s, skip(&s, 0), 0).and_then(|end| match skip(&s, end) {
        end if end == s.len() => Ok(()),
        end => fault("Extra data", end),
    });
    let Fault { message, at } = found.err()?;
    let before = &s[..at.min(s.len())];
    let line = before.iter().filter(|c| **c == '\n').count() + 1;
    let column = match before.iter().rposition(|c| *c == '\n') {
        Some(newline) => at - newline,
        None => at + 1,
    };
    Some(format!("{message}: line {line} column {column} (char {at})"))
}

#[cfg(test)]
mod tests {
    use super::error;

    #[test]
    fn messages_of_python() {
        for (text, message) in [
            ("[1, 2,", "Expecting value: line 1 column 7 (char 6)"),
            ("{x", "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ("", "Expecting value: line 1 column 1 (char 0)"),
            ("[1 2]", "Expecting ',' delimiter: line 1 column 4 (char 3)"),
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("[1]\n x", "Extra data: line 2 column 2 (char 5)"),
            ("[\"a", "Unterminated string starting at: line 1 column 2 (char 1)"),
            ("[\"\\q\"]", "Invalid \\escape: line 1 column 3 (char 2)"),
            ("[\"\\u12g4\"]", "Invalid \\uXXXX escape: line 1 column 4 (char 3)"),
            ("[\"a\tb\"]", "Invalid control character at: line 1 column 4 (char 3)"),
            ("[01]", "Expecting ',' delimiter: line 1 column 3 (char 2)"),
            ("[-]", "Expecting value: line 1 column 2 (char 1)"),
            ("{\"a\": 1,}", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
        ] {
            assert_eq!(error(text).as_deref(), Some(message), "{text:?}");
        }
        for text in ["[1, 2.5e3, \"a\\u00e9\", null, {\"k\": [true]}]", " 7 ", "[NaN, -Infinity]"] {
            assert_eq!(error(text), None, "{text:?}");
        }
    }
}
