//! What a typed line asks for.

use fgds_core::serde_json::{json, Value};

/// The first words the prompt knows.
pub const WORDS: &[&str] = &[
    "help",
    "cmd",
    "cmds",
    "events",
    "channels",
    "dict",
    "mute",
    "unmute",
    "subscribe",
    "unsubscribe",
    "filters",
    "clear-filters",
    "raw-uplink",
    "quit",
    "exit",
];

/// The three lists of the dictionary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum List {
    Commands,
    Events,
    Channels,
}

#[derive(Debug, PartialEq)]
pub enum Line {
    Empty,
    Help(Option<String>),
    Cmd {
        name: String,
        args: Vec<String>,
    },
    RawUplink(Vec<u8>),
    Dict(List, Option<String>),
    /// `true` to mute, `false` to unmute; the target is `events`, `channels` or a pattern.
    Mute(bool, String),
    /// `true` to subscribe, `false` to unsubscribe.
    Subscribe(bool, String),
    Filters,
    ClearFilters,
    Quit,
}

/// The words of a line. Quotes (`"` or `'`) keep spaces in a word; `\` keeps the next character.
pub fn words(line: &str) -> Result<Vec<String>, String> {
    let (mut words, mut word, mut open, mut quote) = (Vec::new(), String::new(), false, None);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', Some('"') | None) => {
                word.push(chars.next().ok_or("a `\\` ends the line")?);
                open = true;
            }
            (c, Some(q)) if c == q => quote = None,
            (c, Some(_)) => word.push(c),
            ('"' | '\'', None) => (quote, open) = (Some(c), true),
            (c, None) if c.is_whitespace() => {
                if open {
                    words.push(std::mem::take(&mut word));
                }
                open = false;
            }
            (c, None) => {
                word.push(c);
                open = true;
            }
        }
    }
    if let Some(quote) = quote {
        return Err(format!("a {quote} is not closed"));
    }
    if open {
        words.push(word);
    }
    Ok(words)
}

fn hex(text: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<u32> = text
        .chars()
        .map(|c| c.to_digit(16).ok_or(format!("`{c}` is not a hexadecimal digit")))
        .collect::<Result<_, _>>()?;
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return Err("the bytes are pairs of hexadecimal digits".into());
    }
    Ok(digits.chunks(2).map(|pair| (pair[0] * 16 + pair[1]) as u8).collect())
}

pub fn parse(line: &str) -> Result<Line, String> {
    let words = words(line)?;
    let Some((first, rest)) = words.split_first() else { return Ok(Line::Empty) };
    let at_most = |count: usize, usage: &str| {
        if rest.len() > count {
            Err(format!("usage: {usage}"))
        } else {
            Ok(rest.first().cloned())
        }
    };
    let one = |usage: &str| at_most(1, usage)?.ok_or(format!("usage: {usage}"));
    Ok(match first.as_str() {
        "help" | "?" => Line::Help(at_most(1, "help [name]")?),
        "quit" | "exit" => Line::Quit,
        "cmd" => {
            let (name, args) = rest.split_first().ok_or("usage: cmd <name> [argument ...]")?;
            Line::Cmd { name: name.clone(), args: args.to_vec() }
        }
        "raw-uplink" => {
            if rest.is_empty() {
                return Err("usage: raw-uplink <hexadecimal bytes>".into());
            }
            Line::RawUplink(hex(&rest.concat())?)
        }
        "cmds" => Line::Dict(List::Commands, at_most(1, "cmds [pattern]")?),
        "events" => Line::Dict(List::Events, at_most(1, "events [pattern]")?),
        "channels" => Line::Dict(List::Channels, at_most(1, "channels [pattern]")?),
        "dict" => {
            let usage = "usage: dict commands|events|channels [pattern]";
            let list = match rest.first().map(String::as_str) {
                Some("commands" | "cmds") => List::Commands,
                Some("events") => List::Events,
                Some("channels") => List::Channels,
                _ => return Err(usage.into()),
            };
            if rest.len() > 2 {
                return Err(usage.into());
            }
            Line::Dict(list, rest.get(1).cloned())
        }
        "mute" => Line::Mute(true, one("mute events|channels|<pattern>")?),
        "unmute" => Line::Mute(false, one("unmute events|channels|<pattern>")?),
        "subscribe" => Line::Subscribe(true, one("subscribe <pattern>")?),
        "unsubscribe" => Line::Subscribe(false, one("unsubscribe <pattern>")?),
        "filters" => Line::Filters,
        "clear-filters" => Line::ClearFilters,
        other => return Err(format!("`{other}` is not a word of this prompt (try `help`)")),
    })
}

/// The `args` of a command request. When every argument is `<parameter>=<value>` they are given
/// by name, in any order; otherwise they are the values in order, as `fgds command-send` takes them.
pub fn arguments(args: &[String], parameters: &[String]) -> Value {
    let named: Option<Vec<(&str, &str)>> =
        args.iter().map(|arg| arg.split_once('=').filter(|(name, _)| parameters.iter().any(|p| p == name))).collect();
    match named {
        Some(named) if !named.is_empty() => {
            Value::Object(named.into_iter().map(|(name, value)| (name.to_string(), json!(value))).collect())
        }
        _ => json!(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_split_on_spaces_outside_quotes() {
        assert_eq!(words("  cmd  A.b.C 1  2 ").unwrap(), ["cmd", "A.b.C", "1", "2"]);
        assert_eq!(
            words(r#"cmd A "hello world" 'it "is"' a\ b """#).unwrap(),
            ["cmd", "A", "hello world", "it \"is\"", "a b", ""]
        );
        assert_eq!(words(r#"x "a \" b""#).unwrap(), ["x", "a \" b"]);
        assert_eq!(words(r#"x '{"a": [1, 2]}'"#).unwrap(), ["x", r#"{"a": [1, 2]}"#]);
        assert!(words("").unwrap().is_empty());
        assert!(words("cmd \"open").is_err());
        assert!(words("cmd \\").is_err());
    }

    #[test]
    fn lines_are_read() {
        assert_eq!(parse("   ").unwrap(), Line::Empty);
        assert_eq!(parse("help").unwrap(), Line::Help(None));
        assert_eq!(parse("help A.b.C").unwrap(), Line::Help(Some("A.b.C".into())));
        assert_eq!(
            parse("cmd A.b.C 1 \"two words\"").unwrap(),
            Line::Cmd { name: "A.b.C".into(), args: vec!["1".into(), "two words".into()] }
        );
        assert_eq!(parse("cmds").unwrap(), Line::Dict(List::Commands, None));
        assert_eq!(parse("events *Ping*").unwrap(), Line::Dict(List::Events, Some("*Ping*".into())));
        assert_eq!(parse("dict channels cpu").unwrap(), Line::Dict(List::Channels, Some("cpu".into())));
        assert_eq!(parse("mute channels").unwrap(), Line::Mute(true, "channels".into()));
        assert_eq!(parse("unmute Ref.*").unwrap(), Line::Mute(false, "Ref.*".into()));
        assert_eq!(parse("subscribe CdhCore.*").unwrap(), Line::Subscribe(true, "CdhCore.*".into()));
        assert_eq!(parse("unsubscribe CdhCore.*").unwrap(), Line::Subscribe(false, "CdhCore.*".into()));
        assert_eq!(parse("filters").unwrap(), Line::Filters);
        assert_eq!(parse("clear-filters").unwrap(), Line::ClearFilters);
        assert_eq!(parse("quit").unwrap(), Line::Quit);
        assert_eq!(parse("exit").unwrap(), Line::Quit);
        assert_eq!(parse("raw-uplink 00 01ff").unwrap(), Line::RawUplink(vec![0, 1, 255]));
    }

    #[test]
    fn lines_that_are_refused() {
        for line in [
            "cmd",
            "nonsense",
            "mute",
            "mute a b",
            "subscribe",
            "dict",
            "dict things",
            "raw-uplink",
            "raw-uplink 0",
            "raw-uplink zz",
            "help a b",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn arguments_are_given_in_order_or_by_name() {
        let parameters = ["arg1".to_string(), "arg2".to_string()];
        let args = |texts: &[&str]| texts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(arguments(&args(&["1", "2"]), &parameters), json!(["1", "2"]));
        assert_eq!(arguments(&args(&["arg2=2", "arg1=1"]), &parameters), json!({"arg2": "2", "arg1": "1"}));
        // A value that only looks like a name stays a value.
        assert_eq!(arguments(&args(&["x=1", "arg1=1"]), &parameters), json!(["x=1", "arg1=1"]));
        assert_eq!(arguments(&[], &parameters), json!([]));
    }
}
