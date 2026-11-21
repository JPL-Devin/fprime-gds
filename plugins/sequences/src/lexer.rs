//! The terminals of the sequence grammar, matched the way Lark's contextual lexer matches them.
//!
//! In each parser state only the terminals the state accepts are tried, in a fixed order, and
//! the first that matches wins (not the longest). Each matcher returns the length the regular
//! expression of the Python grammar would match, backtracking included.

use crate::tables::{IGNORED, SYMBOLS, TERMINALS};

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// Index in `SYMBOLS`.
    pub kind: u8,
    pub text: String,
    pub line: usize,
    pub column: usize,
}

pub fn symbol(name: &str) -> u8 {
    SYMBOLS.iter().position(|s| *s == name).expect("a symbol of the grammar") as u8
}

fn digit(c: Option<&char>) -> bool {
    c.is_some_and(char::is_ascii_digit)
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `[0-9](_?[0-9])*`
fn digits(s: &[char], mut i: usize) -> Option<usize> {
    if !digit(s.get(i)) {
        return None;
    }
    i += 1;
    loop {
        if digit(s.get(i)) {
            i += 1;
        } else if s.get(i) == Some(&'_') && digit(s.get(i + 1)) {
            i += 2;
        } else {
            return Some(i);
        }
    }
}

fn sign(s: &[char]) -> usize {
    usize::from(matches!(s.first(), Some('+' | '-')))
}

/// `[eE][+-]?D`
fn exponent(s: &[char], i: usize) -> Option<usize> {
    if !matches!(s.get(i), Some('e' | 'E')) {
        return None;
    }
    digits(s, i + 1 + sign(&s[i + 1..]))
}

fn float(s: &[char]) -> Option<usize> {
    let start = sign(s);
    let whole = digits(s, start);
    if let Some(end) = whole.and_then(|i| exponent(s, i)) {
        return Some(end);
    }
    let dot = whole.unwrap_or(start);
    let fraction = match s.get(dot) {
        Some('.') => digits(s, dot + 1).or_else(|| digits(s, start + 1).filter(|_| s.get(start) == Some(&'.')))?,
        _ => return None,
    };
    Some(exponent(s, fraction).unwrap_or(fraction))
}

fn decimal(s: &[char]) -> Option<usize> {
    let start = sign(s);
    match s.get(start) {
        Some('0') => {
            // `0(_?0)*(?![1-9xX])`: the longest run of zeros not followed by a digit or `x`.
            let mut ends = vec![start + 1];
            let mut i = start + 1;
            loop {
                if s.get(i) == Some(&'0') {
                    i += 1;
                } else if s.get(i) == Some(&'_') && s.get(i + 1) == Some(&'0') {
                    i += 2;
                } else {
                    break;
                }
                ends.push(i);
            }
            ends.into_iter().rev().find(|&end| !matches!(s.get(end), Some('1'..='9' | 'x' | 'X')))
        }
        Some('1'..='9') => digits(s, start),
        _ => None,
    }
}

fn hex(s: &[char]) -> Option<usize> {
    let mut i = sign(s);
    if s.get(i) != Some(&'0') || !matches!(s.get(i + 1), Some('x' | 'X')) {
        return None;
    }
    i += 2;
    let first = i;
    loop {
        let hex = |c: Option<&char>| c.is_some_and(char::is_ascii_hexdigit);
        if hex(s.get(i)) {
            i += 1;
        } else if s.get(i) == Some(&'_') && hex(s.get(i + 1)) {
            i += 2;
        } else {
            return (i > first).then_some(i);
        }
    }
}

/// A quoted string on one line. The closing quote is the first one not escaped by a
/// backslash; the text is taken as written, escapes included.
fn string(s: &[char]) -> Option<usize> {
    let quote = *s.first().filter(|c| matches!(c, '"' | '\''))?;
    if s.get(1) == Some(&quote) && s.get(2) == Some(&quote) {
        return None;
    }
    let mut i = 1;
    loop {
        if s[i - 1] != '\\' {
            let mut j = i;
            while s.get(j) == Some(&'\\') && s.get(j + 1) == Some(&'\\') {
                j += 2;
            }
            if s.get(j) == Some(&quote) {
                // The shortest match: the first number of backslash pairs that reaches a quote.
                let mut k = i;
                while s.get(k) != Some(&quote) {
                    k += 2;
                }
                return Some(k + 1);
            }
        }
        match s.get(i) {
            Some('\n') | None => return None,
            Some(_) => i += 1,
        }
    }
}

fn name(s: &[char]) -> Option<usize> {
    let mut i = usize::from(s.first() == Some(&'$'));
    // `[^\W\d]`: a letter or an underscore.
    if !s.get(i).is_some_and(|c| word(*c) && !c.is_numeric()) {
        return None;
    }
    let run = |mut i: usize| {
        while s.get(i).is_some_and(|c| word(*c)) {
            i += 1;
        }
        i
    };
    i = run(i + 1);
    while s.get(i) == Some(&'.') && s.get(i + 1).is_some_and(|c| word(*c)) {
        i = run(i + 1);
    }
    Some(i)
}

fn comment(s: &[char]) -> Option<usize> {
    (s.first() == Some(&';')).then(|| s.iter().position(|c| *c == '\n').unwrap_or(s.len()))
}

fn newline(s: &[char]) -> Option<usize> {
    let mut i = 0;
    loop {
        if s.get(i) == Some(&'\n') {
            i += 1;
            while matches!(s.get(i), Some('\t' | ' ')) {
                i += 1;
            }
        } else if let Some(n) = comment(&s[i..]) {
            i += n;
        } else {
            return (i > 0).then_some(i);
        }
    }
}

fn blanks(s: &[char]) -> usize {
    s.iter().take_while(|c| matches!(c, '\t' | ' ' | '\x0c')).count()
}

fn continuation(s: &[char]) -> Option<usize> {
    if s.first() != Some(&'\\') {
        return None;
    }
    let i = 1 + blanks(&s[1..]);
    (s.get(i) == Some(&'\n')).then_some(i + 1)
}

/// Digits and separators in the shape of `pattern`, where `d` stands for a digit; then,
/// when `fraction`, a dot and one to six digits.
fn time(s: &[char], pattern: &str, fraction: bool) -> Option<usize> {
    let mut i = 0;
    for p in pattern.chars() {
        let c = s.get(i)?;
        if !(if p == 'd' { c.is_ascii_digit() } else { *c == p }) {
            return None;
        }
        i += 1;
    }
    if fraction {
        if s.get(i) != Some(&'.') || !digit(s.get(i + 1)) {
            return None;
        }
        let digits = s[i + 1..].iter().take(6).take_while(|c| c.is_ascii_digit()).count();
        i += 1 + digits;
    }
    Some(i)
}

fn literal(s: &[char], c: char) -> Option<usize> {
    (s.first() == Some(&c)).then_some(1)
}

fn keyword(s: &[char], words: [&str; 3]) -> Option<usize> {
    words.iter().find(|w| s.len() >= w.len() && s.iter().zip(w.chars()).all(|(a, b)| *a == b)).map(|w| w.len())
}

fn match_terminal(kind: usize, s: &[char]) -> Option<usize> {
    const HMS: &str = "dd:dd:dd";
    const DATE: &str = "dddd-dddTdd:dd:dd";
    match SYMBOLS[kind] {
        "CONST_FALSE" => keyword(s, ["False", "false", "FALSE"]),
        "CONST_TRUE" => keyword(s, ["True", "true", "TRUE"]),
        "STRING" => string(s),
        "HEX_NUMBER" => hex(s),
        "_NEWLINE" => newline(s),
        "NAME" => name(s),
        "__IGNORE_1" => continuation(s),
        "COMMENT" => comment(s),
        "__IGNORE_0" => Some(blanks(s)).filter(|n| *n > 0),
        "ABS_DATETIME_FRAC" => time(s, DATE, true),
        "ABS_DATETIME" => time(s, DATE, false),
        "TIME_HMS_FRAC" => time(s, HMS, true),
        "TIME_HMS" => time(s, HMS, false),
        "A" => literal(s, 'A'),
        "COLON" => literal(s, ':'),
        "COMMA" => literal(s, ','),
        "LBRACE" => literal(s, '{'),
        "LSQB" => literal(s, '['),
        "R" => literal(s, 'R'),
        "RBRACE" => literal(s, '}'),
        "RSQB" => literal(s, ']'),
        "FLOAT_NUMBER" => float(s),
        "DEC_NUMBER" => decimal(s),
        other => unreachable!("{other} is not a terminal"),
    }
}

pub struct Lexer {
    text: Vec<char>,
    pos: usize,
    line: usize,
    line_start: usize,
}

/// Why no token was produced.
pub enum Stop {
    End,
    /// The text here is not a terminal the parser accepts; the token is what it is in
    /// any context, when it is one.
    Unexpected {
        line: usize,
        column: usize,
        token: Option<Token>,
    },
}

impl Lexer {
    pub fn new(text: &str) -> Self {
        Lexer { text: text.chars().collect(), pos: 0, line: 1, line_start: 0 }
    }

    /// The first terminal of `accepted` (a predicate on terminal indexes) that matches here.
    fn scan(&self, accepted: &dyn Fn(u8) -> bool) -> Option<(u8, usize)> {
        let rest = &self.text[self.pos..];
        let (name, r, a) = (symbol("NAME"), symbol("R"), symbol("A"));
        // Where a name is accepted, `R` and `A` are names that Lark renames afterwards.
        let keywords_are_names = accepted(name);
        for kind in 0..TERMINALS as u8 {
            if !accepted(kind) || (keywords_are_names && (kind == r || kind == a)) {
                continue;
            }
            if let Some(len) = match_terminal(kind as usize, rest) {
                let kind = match (kind == name && len == 1, rest[0]) {
                    (true, 'R') if accepted(r) => r,
                    (true, 'A') if accepted(a) => a,
                    _ => kind,
                };
                return Some((kind, len));
            }
        }
        None
    }

    fn take(&mut self, kind: u8, len: usize) -> Token {
        let text = &self.text[self.pos..self.pos + len];
        let token =
            Token { kind, text: text.iter().collect(), line: self.line, column: self.pos - self.line_start + 1 };
        if let Some(last) = text.iter().rposition(|c| *c == '\n') {
            self.line += text.iter().filter(|c| **c == '\n').count();
            self.line_start = self.pos + last + 1;
        }
        self.pos += len;
        token
    }

    /// The next token among the terminals `accepted` by the parser state.
    pub fn next(&mut self, accepted: &dyn Fn(u8) -> bool) -> Result<Token, Stop> {
        let wanted = |kind: u8| accepted(kind) || IGNORED.contains(&kind);
        while self.pos < self.text.len() {
            let Some((kind, len)) = self.scan(&wanted) else {
                let (line, column) = (self.line, self.pos - self.line_start + 1);
                let token = self.scan(&|_| true).map(|(kind, len)| self.take(kind, len));
                return Err(Stop::Unexpected { line, column, token });
            };
            let token = self.take(kind, len);
            if !IGNORED.contains(&kind) {
                return Ok(token);
            }
        }
        Err(Stop::End)
    }
}
