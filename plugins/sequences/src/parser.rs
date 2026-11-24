//! The sequence file parser: the LALR(1) tables of the Python GDS grammar (`tables.rs`),
//! run over the contextual lexer, building the statements as the rules are reduced.

use crate::lexer::{symbol, Lexer, Stop, Token};
use crate::tables::{Action, ACCEPT, END, RULES, START, STATES, SYMBOLS};
use crate::value::{Int, Value};

/// One command of a sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// Line of the time tag, from 1.
    pub line: usize,
    pub relative: bool,
    /// The time as written, without the leading `R` or `A`.
    pub time: String,
    pub mnemonic: String,
    /// An array or object argument is a `Value::Array` or `Value::Object`.
    pub args: Vec<Value>,
    /// Set when a quoted string inside an array or object starts like JSON and is not JSON.
    pub arg_error: Option<String>,
}

/// Where the text stops being a sequence. `token` is the terminal name and text found there,
/// when the text is a token of the grammar that is not allowed at this place.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
    pub line: usize,
    pub column: usize,
    pub token: Option<(&'static str, String)>,
}

enum Node {
    None,
    Token(Token),
    Value(Value),
    Values(Vec<Value>),
    Pair(String, Value),
    Pairs(Vec<(String, Value)>),
    Time { relative: bool, token: Token },
}

fn action(state: u8, sym: u8) -> Option<Action> {
    let row = STATES[state as usize];
    row.binary_search_by_key(&sym, |(s, _)| *s).ok().map(|i| row[i].1)
}

/// A string inside an array or object that starts like a JSON array or object is read as
/// one, as the Python parser does.
fn nested(value: Value, rule: &str, error: &mut Option<String>) -> Value {
    match value {
        Value::Str(s) if s.starts_with(['[', '{']) => Value::parse_json(&s).unwrap_or_else(|e| {
            error.get_or_insert(format!("Error trying to process rule \"{rule}\":\n\n{e}"));
            Value::Str(s)
        }),
        other => other,
    }
}

struct Builder {
    statements: Vec<Statement>,
    /// Error of the arguments reduced since the last statement.
    arg_error: Option<String>,
}

impl Builder {
    fn reduce(&mut self, origin: &str, children: Vec<Node>) -> Node {
        let mut children = children.into_iter();
        match origin {
            "number" | "string" | "boolean" => match children.next().unwrap_or(Node::None) {
                Node::Token(t) => Node::Value(match SYMBOLS[t.kind as usize] {
                    "DEC_NUMBER" => Int::parse(&t.text, 10).map_or(Value::Null, Value::Int),
                    "HEX_NUMBER" => {
                        let digits = t.text.replacen("0x", "", 1).replacen("0X", "", 1);
                        Int::parse(&digits, 16).map_or(Value::Null, Value::Int)
                    }
                    // A literal beyond a double stays text, which no numeric argument takes.
                    "FLOAT_NUMBER" => match t.text.replace('_', "").parse::<f64>() {
                        Ok(v) if v.is_finite() => Value::Float(v),
                        _ => Value::Str(t.text),
                    },
                    "STRING" => Value::Str(t.text.chars().skip(1).take(t.text.chars().count() - 2).collect()),
                    kind => Value::Bool(kind == "CONST_TRUE"),
                }),
                other => other,
            },
            "value" => match children.next().unwrap_or(Node::None) {
                Node::Token(t) => Node::Value(Value::Str(t.text)),
                other => other,
            },
            "_value_list" | "___value_list_star_1" | "__array_star_2" | "array" => {
                let mut values = Vec::new();
                for child in children {
                    match child {
                        Node::Value(v) => values.push(v),
                        Node::Values(vs) => values.extend(vs),
                        _ => {}
                    }
                }
                if origin == "array" {
                    let items = values.into_iter().map(|v| nested(v, "array", &mut self.arg_error)).collect();
                    Node::Value(Value::Array(items))
                } else {
                    Node::Values(values)
                }
            }
            "kv" => match (children.next().unwrap_or(Node::None), children.nth(1)) {
                (Node::Token(key), Some(Node::Value(value))) => Node::Pair(key.text, value),
                _ => Node::None,
            },
            "__object_star_3" | "object" => {
                let mut pairs = Vec::new();
                for child in children {
                    match child {
                        Node::Pair(k, v) => pairs.push((k, v)),
                        Node::Pairs(ps) => pairs.extend(ps),
                        _ => {}
                    }
                }
                if origin == "object" {
                    let pairs: Vec<_> =
                        pairs.into_iter().map(|(k, v)| (k, nested(v, "object", &mut self.arg_error))).collect();
                    Node::Value(Value::object(pairs))
                } else {
                    Node::Pairs(pairs)
                }
            }
            "relative_time" | "absolute_time" => match children.nth(1) {
                Some(Node::Token(token)) => Node::Time { relative: origin == "relative_time", token },
                _ => Node::None,
            },
            "_cmd_stmt" => {
                let (time, mnemonic) = (children.next().unwrap_or(Node::None), children.next().unwrap_or(Node::None));
                let mut args = Vec::new();
                for child in children {
                    match child {
                        Node::Value(v) => args.push(v),
                        Node::Values(vs) => args.extend(vs),
                        _ => {}
                    }
                }
                if let (Node::Time { relative, token }, Node::Token(name)) = (time, mnemonic) {
                    self.statements.push(Statement {
                        line: token.line,
                        relative,
                        time: token.text,
                        mnemonic: name.text,
                        args,
                        arg_error: self.arg_error.take(),
                    });
                }
                Node::None
            }
            // Rules with one symbol hand it on: time tags, literals, the mnemonic.
            _ if children.len() == 1 => children.next().unwrap_or(Node::None),
            _ => Node::None,
        }
    }
}

/// Parse the text of a sequence file. Line ends are `\n`, `\r\n` or `\r`.
pub fn parse(text: &str) -> Result<Vec<Statement>, SyntaxError> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lexer = Lexer::new(&text);
    let mut builder = Builder { statements: Vec::new(), arg_error: None };
    let mut states = vec![START];
    let mut nodes: Vec<Node> = Vec::new();
    // Line and column of the last token: where Lark reports an early end of file.
    let mut last = (1, 1);
    let (comma, lsqb, lbrace) = (symbol("COMMA"), symbol("LSQB"), symbol("LBRACE"));
    loop {
        let state = *states.last().unwrap_or(&START);
        let token = match lexer.next(&|kind| action(state, kind).is_some()) {
            Ok(token) => token,
            Err(Stop::End) => Token { kind: END, text: String::new(), line: last.0, column: last.1 },
            Err(Stop::Unexpected { line, column, token }) => {
                let token = token.map(|t| (SYMBOLS[t.kind as usize], t.text));
                return Err(SyntaxError { line, column, token });
            }
        };
        if token.kind != END {
            last = (token.line, token.column);
        }
        // Reduce until the token is shifted.
        loop {
            let state = *states.last().unwrap_or(&START);
            match action(state, token.kind) {
                Some(Action::Shift(next)) => {
                    states.push(next);
                    // Punctuation is not part of the tree, except where a rule looks at positions.
                    let keep = ![comma, lsqb, lbrace].contains(&token.kind);
                    nodes.push(if keep { Node::Token(token) } else { Node::None });
                    break;
                }
                Some(Action::Reduce(rule)) => {
                    let (origin, len) = RULES[rule as usize];
                    let at = nodes.len().saturating_sub(len as usize);
                    let children = nodes.split_off(at);
                    states.truncate(states.len().saturating_sub(len as usize).max(1));
                    let node = builder.reduce(SYMBOLS[origin as usize], children);
                    nodes.push(node);
                    let state = *states.last().unwrap_or(&START);
                    match action(state, origin) {
                        Some(Action::Shift(next)) => states.push(next),
                        _ => unreachable!("the tables have a goto for every reduction"),
                    }
                    if states.last() == Some(&ACCEPT) && token.kind == END {
                        return Ok(builder.statements);
                    }
                }
                None => {
                    let token = Some((SYMBOLS[token.kind as usize], token.text));
                    return Err(SyntaxError { line: last.0, column: last.1, token });
                }
            }
        }
    }
}
