//! The sequence file parser. One command a line:
//!
//! ```text
//! R00:00:01.5 component.COMMAND 1, 0x10, 2.5, "text", NAME, true, [1, 2], {x: 1, y: [2, 3]} ; comment
//! A2025-015T12:00:00 component.COMMAND
//! ```
//!
//! The commas between the arguments of a command are optional; those inside an array or an object are
//! not. A `\` at the end of a line continues it on the next.

use fgds_core::dictionary::input;
use fgds_core::serde_json::{json, Map, Value};

/// One command of a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// Line of the time tag, from 1.
    pub line: usize,
    pub relative: bool,
    /// The time as written, without the leading `R` or `A`.
    pub time: String,
    pub mnemonic: String,
    /// A number is a JSON number, a name or a quoted text a JSON string (a number literal no double
    /// holds stays text, and is refused as a value).
    pub args: Vec<Value>,
    /// Set when a quoted text inside an array or object starts like JSON and is not JSON.
    pub arg_error: Option<String>,
}

/// Where the text stops being a sequence, and the character found there.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub found: Option<char>,
}

struct Parser {
    text: Vec<char>,
    at: usize,
    line: usize,
    column: usize,
    arg_error: Option<String>,
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.text.get(self.at).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        (self.line, self.column) = if c == '\n' { (self.line + 1, 1) } else { (self.line, self.column + 1) };
        Some(c)
    }

    fn error<T>(&self) -> Result<T, SyntaxError> {
        Err(SyntaxError { line: self.line, column: self.column, found: self.peek() })
    }

    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        if found {
            self.bump();
        }
        found
    }

    fn expect(&mut self, c: char) -> Result<(), SyntaxError> {
        if self.eat(c) {
            Ok(())
        } else {
            self.error()
        }
    }

    /// Characters while `keep` holds.
    fn run(&mut self, keep: impl Fn(char) -> bool) -> String {
        let start = self.at;
        while self.peek().is_some_and(&keep) {
            self.bump();
        }
        self.text[start..self.at].iter().collect()
    }

    /// Blanks, and a `\` that ends its line.
    fn blanks(&mut self) {
        loop {
            self.run(|c| matches!(c, ' ' | '\t' | '\x0c'));
            let rest = &self.text[self.at..];
            let continued = rest.first() == Some(&'\\')
                && rest[1..].iter().find(|c| !matches!(c, ' ' | '\t' | '\x0c')) == Some(&'\n');
            if !continued {
                return;
            }
            while self.bump() != Some('\n') {}
        }
    }

    /// The end of a statement: a comment, the end of the line or of the text.
    fn at_end(&self) -> bool {
        matches!(self.peek(), None | Some('\n' | ';'))
    }

    fn digits(&mut self, count: usize) -> Result<(), SyntaxError> {
        for _ in 0..count {
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return self.error();
            }
            self.bump();
        }
        Ok(())
    }

    /// `HH:MM:SS[.ffffff]`, after `YYYY-DDDT` when absolute.
    fn time(&mut self, relative: bool) -> Result<String, SyntaxError> {
        let start = self.at;
        if !relative {
            self.digits(4)?;
            self.expect('-')?;
            self.digits(3)?;
            self.expect('T')?;
        }
        self.digits(2)?;
        self.expect(':')?;
        self.digits(2)?;
        self.expect(':')?;
        self.digits(2)?;
        if self.eat('.') {
            self.digits(1)?;
            for _ in 0..5 {
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.bump();
                }
            }
        }
        Ok(self.text[start..self.at].iter().collect())
    }

    /// A name: a letter or `_`, then letters, digits and `_`, in parts joined by `.`; `$` may lead.
    fn name(&mut self) -> Result<String, SyntaxError> {
        let start = self.at;
        self.eat('$');
        if !self.peek().is_some_and(|c| word(c) && !c.is_numeric()) {
            return self.error();
        }
        loop {
            self.run(word);
            let part = self.peek() == Some('.') && self.text.get(self.at + 1).is_some_and(|c| word(*c));
            if !part {
                break;
            }
            self.bump();
        }
        Ok(self.text[start..self.at].iter().collect())
    }

    /// A quoted text, kept as written between the quotes: a `\` before a quote keeps both.
    fn string(&mut self, quote: char) -> Result<Value, SyntaxError> {
        self.bump();
        let start = self.at;
        let mut escaped = false;
        loop {
            match self.peek() {
                None | Some('\n') => return self.error(),
                Some(c) if c == quote && !escaped => break,
                Some(c) => escaped = c == '\\' && !escaped,
            }
            self.bump();
        }
        let text: String = self.text[start..self.at].iter().collect();
        self.bump();
        Ok(json!(text))
    }

    /// A decimal or hexadecimal integer or a decimal number, with a sign.
    fn number(&mut self) -> Result<Value, SyntaxError> {
        let (line, column, start) = (self.line, self.column, self.at);
        if matches!(self.peek(), Some('+' | '-')) {
            self.bump();
        }
        let mut last = ' ';
        while let Some(c) = self.peek() {
            let hex = self.text[start..self.at].iter().any(|c| matches!(c, 'x' | 'X'));
            let exponent_sign = matches!(c, '+' | '-') && matches!(last, 'e' | 'E') && !hex;
            if !(word(c) || c == '.' || exponent_sign) {
                break;
            }
            last = c;
            self.bump();
        }
        let text: String = self.text[start..self.at].iter().collect();
        let body = text.trim_start_matches(['+', '-']).to_ascii_lowercase();
        let integer = match body.starts_with("0o") || body.starts_with("0b") {
            true => None,
            false => input::integer(&text),
        };
        let decimal = !body.contains('x') && body.contains(['.', 'e']);
        let value = match (integer, input::float(&text).filter(|_| decimal)) {
            (Some(i), _) => match (i64::try_from(i), u64::try_from(i)) {
                (Ok(i), _) => json!(i),
                (_, Ok(u)) => json!(u),
                // Beyond every integer type: a float may still take it.
                _ => json!(i as f64),
            },
            (None, Some(f)) if f.is_finite() => json!(f),
            // A decimal number no double holds: refused where it is used, with the command named.
            (None, _) if decimal => {
                let shape = body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-' | '_'));
                match shape && text.replace('_', "").parse::<f64>().is_ok() {
                    true => json!(text),
                    false => return Err(SyntaxError { line, column, found: text.chars().next() }),
                }
            }
            _ => return Err(SyntaxError { line, column, found: text.chars().next() }),
        };
        Ok(value)
    }

    /// A value inside an array or object; a quoted text that starts like a JSON array or object is one.
    fn member(&mut self) -> Result<Value, SyntaxError> {
        let value = self.value()?;
        Ok(match value {
            Value::String(s) if s.starts_with(['[', '{']) => match fgds_core::serde_json::from_str(&s) {
                Ok(parsed) => parsed,
                Err(e) => {
                    self.arg_error.get_or_insert(format!("{s:?} is not JSON: {e}"));
                    Value::String(s)
                }
            },
            other => other,
        })
    }

    /// The members of an array or object up to `close`, with a comma between two and maybe after the last.
    fn members<T>(
        &mut self,
        close: char,
        mut member: impl FnMut(&mut Self) -> Result<T, SyntaxError>,
    ) -> Result<Vec<T>, SyntaxError> {
        self.bump();
        let mut out = Vec::new();
        loop {
            self.blanks();
            if self.eat(close) {
                return Ok(out);
            }
            out.push(member(self)?);
            self.blanks();
            if !self.eat(',') {
                self.blanks();
                return self.expect(close).map(|()| out);
            }
        }
    }

    fn value(&mut self) -> Result<Value, SyntaxError> {
        match self.peek() {
            Some(quote @ ('"' | '\'')) => self.string(quote),
            Some('[') => self.members(']', Self::member).map(Value::Array),
            Some('{') => {
                let pair = |p: &mut Self| {
                    let key = p.name()?;
                    p.blanks();
                    p.expect(':')?;
                    p.blanks();
                    Ok((key, p.member()?))
                };
                // A key written twice keeps its first place and its last value.
                self.members('}', pair).map(|pairs| Value::Object(pairs.into_iter().collect::<Map<_, _>>()))
            }
            Some(c) if c.is_ascii_digit() || matches!(c, '+' | '-' | '.') => self.number(),
            _ => Ok(match self.name()?.as_str() {
                "True" | "true" | "TRUE" => json!(true),
                "False" | "false" | "FALSE" => json!(false),
                name => json!(name),
            }),
        }
    }

    fn statement(&mut self) -> Result<Statement, SyntaxError> {
        let line = self.line;
        let relative = match self.peek() {
            Some('R') => true,
            Some('A') => false,
            _ => return self.error(),
        };
        self.bump();
        self.blanks();
        let time = self.time(relative)?;
        self.blanks();
        let mnemonic = self.name()?;
        let mut args = Vec::new();
        loop {
            self.blanks();
            let comma = self.eat(',');
            self.blanks();
            if self.at_end() {
                if comma {
                    return self.error();
                }
                break;
            }
            args.push(self.value()?);
        }
        Ok(Statement { line, relative, time, mnemonic, args, arg_error: self.arg_error.take() })
    }
}

/// Parse the text of a sequence file. Line ends are `\n`, `\r\n` or `\r`.
pub fn parse(text: &str) -> Result<Vec<Statement>, SyntaxError> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut parser = Parser { text: text.chars().collect(), at: 0, line: 1, column: 1, arg_error: None };
    let mut statements = Vec::new();
    loop {
        parser.blanks();
        match parser.peek() {
            None => return Ok(statements),
            Some('\n') => {
                parser.bump();
            }
            Some(';') => {
                parser.run(|c| c != '\n');
            }
            Some(_) => statements.push(parser.statement()?),
        }
    }
}
