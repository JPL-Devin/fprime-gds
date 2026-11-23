//! A command line read as Python `argparse` reads it (CPython 3.12, `ArgumentParser._parse_known_args`):
//! which token is an option, an argument of an option or a positional, abbreviated long options, values
//! attached to a short option, negative numbers, `--`. A subcommand that stands for a command of
//! `fprime-cli` gives its options here, and the tokens come back spelled so that only one reading is left.

use fgds_dictionary::{is_decimal_digit, python_decimal, python_float};

/// `nargs` of an argument.
#[derive(Clone, Copy, PartialEq)]
pub enum Nargs {
    /// No value (`store_true`).
    Flag,
    /// `None`: one value.
    One,
    /// `'?'`
    Optional,
    /// `'*'`
    ZeroOrMore,
    /// `'+'`
    OneOrMore,
    /// An integer.
    Exactly(usize),
}

/// `type` and `choices` of an argument.
#[derive(Clone, Copy)]
pub enum Kind {
    Str,
    Int,
    Float,
    Choice(&'static [&'static str]),
}

pub struct Opt {
    /// The option strings, `-i` and `--ids`.
    pub strings: Vec<String>,
    pub nargs: Nargs,
    pub kind: Kind,
    /// Whether a prefix of the long option stands for it: so for the options of `fprime-cli`, not for
    /// those `fgds` adds, which leaves every command line of `fprime-cli` read as `fprime-cli` reads it.
    pub abbreviated: bool,
    /// The long option the values are given back with; `None` for an option without a meaning here.
    pub emit: Option<String>,
}

impl Opt {
    pub fn new(strings: &[&str], nargs: Nargs, kind: Kind) -> Opt {
        let emit = strings.iter().find_map(|s| s.strip_prefix("--")).map(str::to_string);
        Opt { strings: strings.iter().map(|s| s.to_string()).collect(), nargs, kind, abbreviated: true, emit }
    }

    /// An option of `fprime-cli` that changes nothing here: read, checked and dropped.
    pub fn ignored(mut self) -> Opt {
        self.emit = None;
        self
    }

    /// An option `fprime-cli` does not have: it is written in full.
    pub fn exact(mut self) -> Opt {
        self.abbreviated = false;
        self
    }

    fn name(&self) -> String {
        self.strings.join("/")
    }
}

pub struct Parser {
    pub options: Vec<Opt>,
    /// The positionals in order: `nargs` and the name shown in messages.
    pub positionals: Vec<(Nargs, &'static str)>,
}

/// One piece of a pattern over the letters `A` (argument), `O` (option), `-` (`--`):
/// the letters, the least and the most of them.
type Piece = (&'static str, usize, usize);

const MANY: usize = usize::MAX;

/// `_get_nargs_pattern`: one group per argument.
fn pattern(nargs: Nargs, option: bool) -> Vec<Piece> {
    let around = |middle: Vec<Piece>, tail: Piece| {
        let mut all = vec![("-", 0, MANY)];
        all.extend(middle);
        all.push(tail);
        all
    };
    match (nargs, option) {
        (Nargs::Flag, _) => vec![],
        (Nargs::One, true) => vec![("A", 1, 1)],
        (Nargs::One, false) => around(vec![("A", 1, 1)], ("-", 0, MANY)),
        (Nargs::Optional, true) => vec![("A", 0, 1)],
        (Nargs::Optional, false) => around(vec![("A", 0, 1)], ("-", 0, MANY)),
        (Nargs::ZeroOrMore, true) => vec![("A", 0, MANY)],
        (Nargs::ZeroOrMore, false) => around(vec![], ("A-", 0, MANY)),
        (Nargs::OneOrMore, true) => vec![("A", 1, MANY)],
        (Nargs::OneOrMore, false) => around(vec![("A", 1, 1)], ("A-", 0, MANY)),
        (Nargs::Exactly(n), true) => vec![("AO", n, n)],
        (Nargs::Exactly(n), false) => {
            (0..n).flat_map(|_| [("-", 0, MANY), ("A", 1, 1)]).chain([("-", 0, MANY)]).collect()
        }
    }
}

/// `re.match` of the groups at the start of `text`: the length of each group, greedy with backtracking.
fn matched(groups: &[Vec<Piece>], text: &[u8]) -> Option<Vec<usize>> {
    fn go(pieces: &[(Piece, usize)], text: &[u8], at: usize, ends: &mut Vec<usize>) -> bool {
        let Some((((letters, least, most), group), rest)) = pieces.split_first() else { return true };
        let run = text[at..].iter().take_while(|c| letters.as_bytes().contains(c)).count().min(*most);
        for taken in (*least..=run).rev() {
            let before = ends[*group];
            ends[*group] = at + taken;
            if go(rest, text, at + taken, ends) {
                return true;
            }
            ends[*group] = before;
        }
        false
    }
    let pieces: Vec<(Piece, usize)> =
        groups.iter().enumerate().flat_map(|(group, pieces)| pieces.iter().map(move |p| (*p, group))).collect();
    let mut ends = vec![usize::MAX; groups.len()];
    if !go(&pieces, text, 0, &mut ends) {
        return None;
    }
    let mut start = 0;
    Some(
        ends.iter()
            .map(|end| {
                // A group without a piece is empty where the one before ended.
                let end = if *end == usize::MAX { start } else { *end };
                let length = end - start;
                start = end;
                length
            })
            .collect(),
    )
}

/// What `_parse_optional` finds for a token: the candidates (option index, option string, whether `=`
/// was written, the attached value), `None` in the index for a token that is an option nobody has.
type Found = Vec<(Option<usize>, String, bool, Option<String>)>;

impl Parser {
    fn lookup(&self, string: &str) -> Option<usize> {
        self.options.iter().position(|o| o.strings.iter().any(|s| s == string))
    }

    /// `_negative_number_matcher`: `^-\d+$|^-\d*\.\d+$`, `\d` of any script, `$` also before a last newline.
    fn negative_number(token: &str) -> bool {
        let token = token.strip_suffix('\n').unwrap_or(token);
        let Some(rest) = token.strip_prefix('-') else { return false };
        let digits = |text: &str| !text.is_empty() && text.chars().all(is_decimal_digit);
        match rest.rsplit_once('.') {
            Some((whole, fraction)) => (whole.is_empty() || digits(whole)) && digits(fraction),
            None => digits(rest),
        }
    }

    /// `_parse_optional`: `None` for a positional.
    fn optional(&self, token: &str) -> Option<Found> {
        if !token.starts_with('-') {
            return None;
        }
        if let Some(index) = self.lookup(token) {
            return Some(vec![(Some(index), token.to_string(), false, None)]);
        }
        if token.chars().count() == 1 {
            return None;
        }
        let (before, after) = match token.split_once('=') {
            Some((before, after)) => (before, Some(after.to_string())),
            None => (token, None),
        };
        if after.is_some() {
            if let Some(index) = self.lookup(before) {
                return Some(vec![(Some(index), before.to_string(), true, after)]);
            }
        }
        // `_get_option_tuples`: the long options the text starts, the short option with its value attached.
        let mut found: Found = Vec::new();
        let double = token.chars().nth(1) == Some('-');
        let short: String = token.chars().take(2).collect();
        let attached: String = token.chars().skip(2).collect();
        for (index, option) in self.options.iter().enumerate() {
            for string in &option.strings {
                if !double && *string == short {
                    found.push((Some(index), string.clone(), false, Some(attached.clone())));
                } else if option.abbreviated && string.starts_with(before) {
                    found.push((Some(index), string.clone(), after.is_some(), after.clone()));
                }
            }
        }
        if !found.is_empty() {
            return Some(found);
        }
        if Self::negative_number(token) || token.contains(' ') {
            return None;
        }
        Some(vec![(None, token.to_string(), false, None)])
    }

    /// `_get_value` and `_check_value`: the value as it is given back, or the message of argparse.
    fn value(kind: Kind, text: &str) -> Result<String, String> {
        let quoted = python_repr(text);
        match kind {
            Kind::Str => Ok(text.to_string()),
            Kind::Int => python_decimal(text).map(|v| v.to_string()).ok_or(format!("invalid int value: {quoted}")),
            Kind::Float => python_float(text).map(|v| v.to_string()).ok_or(format!("invalid float value: {quoted}")),
            Kind::Choice(choices) if choices.contains(&text) => Ok(text.to_string()),
            Kind::Choice(choices) => Err(format!("invalid choice: {quoted} (choose from {})", choices.join(", "))),
        }
    }

    /// The tokens after the subcommand, read as argparse reads them and written again: every option of
    /// `emit` as `--name` or `--name=value`, then `--` and the positionals. The error is the message
    /// argparse ends with (exit code 2).
    pub fn canonical(&self, tokens: &[String]) -> Result<Vec<String>, String> {
        let mut letters = Vec::new();
        let mut options: Vec<Option<Found>> = Vec::new();
        let mut plain = false;
        for token in tokens {
            let found = if plain { None } else { self.optional(token) };
            if !plain && token == "--" {
                plain = true;
                letters.push(b'-');
                options.push(None);
                continue;
            }
            letters.push(if found.is_some() { b'O' } else { b'A' });
            options.push(found);
        }
        let mut given: Vec<Option<Vec<String>>> = self.options.iter().map(|_| None).collect();
        let mut positionals: Vec<String> = Vec::new();
        let mut waiting = 0;
        let mut extras: Vec<String> = Vec::new();
        let last = options.iter().rposition(Option::is_some);

        // `consume_positionals`
        let take_positionals = |start: usize, waiting: &mut usize, positionals: &mut Vec<String>| {
            let left = &self.positionals[*waiting..];
            let mut counts = Vec::new();
            for used in (1..=left.len()).rev() {
                let groups: Vec<Vec<Piece>> = left[..used].iter().map(|(nargs, _)| pattern(*nargs, false)).collect();
                if let Some(lengths) = matched(&groups, &letters[start..]) {
                    let end = start + lengths.iter().sum::<usize>();
                    counts = lengths;
                    if letters.get(end) == Some(&b'O') {
                        while counts.last() == Some(&0) {
                            counts.pop();
                        }
                    }
                    break;
                }
            }
            let mut at = start;
            for count in &counts {
                let mut values: Vec<String> = tokens[at..at + count].to_vec();
                if let Some(dashes) = letters[at..at + count].iter().position(|l| *l == b'-') {
                    values.remove(dashes);
                }
                positionals.extend(values);
                at += count;
            }
            *waiting += counts.len();
            at
        };

        let mut start = 0;
        while let Some(last) = last.filter(|last| start <= *last) {
            let next = (start..=last).find(|i| options[*i].is_some()).expect("an option at or before the last one");
            if start != next {
                let end = take_positionals(start, &mut waiting, &mut positionals);
                if end > start {
                    start = end;
                    continue;
                }
            }
            if options[start].is_none() {
                extras.extend(tokens[start..next].iter().cloned());
                start = next;
            }
            // `consume_optional`
            let found = options[start].as_ref().expect("an option here");
            if found.len() > 1 {
                // `_get_option_tuples`: the matches in the order the option strings were added.
                let names: Vec<&str> = found.iter().map(|f| f.1.as_str()).collect();
                return Err(format!("ambiguous option: {} could match {}", tokens[start], names.join(", ")));
            }
            let (mut index, mut string, mut separator, mut explicit) = found[0].clone();
            let mut taken: Vec<(usize, Vec<String>)> = Vec::new();
            let stop;
            loop {
                let Some(option) = index.map(|i| &self.options[i]) else {
                    extras.push(tokens[start].clone());
                    stop = start + 1;
                    break;
                };
                let refuse = |message: String| format!("argument {}: {message}", option.name());
                let count = |text: &[u8]| {
                    matched(&[pattern(option.nargs, true)], text).map(|l| l[0]).ok_or_else(|| {
                        refuse(match option.nargs {
                            Nargs::One => "expected one argument".to_string(),
                            Nargs::Optional => "expected at most one argument".to_string(),
                            Nargs::OneOrMore => "expected at least one argument".to_string(),
                            Nargs::Exactly(1) => "expected 1 argument".to_string(),
                            Nargs::Exactly(n) => format!("expected {n} arguments"),
                            Nargs::Flag | Nargs::ZeroOrMore => unreachable!("matches nothing at least"),
                        })
                    })
                };
                match explicit.clone() {
                    Some(value) => {
                        let wanted = count(b"A")?;
                        let single = string.chars().nth(1) != Some('-');
                        if wanted == 0 && single && !value.is_empty() {
                            if separator || value.starts_with('-') {
                                return Err(refuse(format!("ignored explicit argument {}", python_repr(&value))));
                            }
                            taken.push((index.expect("an option"), Vec::new()));
                            let mut rest = value.chars();
                            let letter = rest.next().expect("not empty");
                            let rest: String = rest.collect();
                            string = format!("-{letter}");
                            match self.lookup(&string) {
                                Some(other) => {
                                    index = Some(other);
                                    (separator, explicit) = match rest.strip_prefix('=') {
                                        _ if rest.is_empty() => (false, None),
                                        Some(after) => (true, Some(after.to_string())),
                                        None => (false, Some(rest)),
                                    };
                                }
                                None => {
                                    extras.push(format!("-{value}"));
                                    stop = start + 1;
                                    break;
                                }
                            }
                        } else if wanted == 1 {
                            taken.push((index.expect("an option"), vec![value]));
                            stop = start + 1;
                            break;
                        } else {
                            return Err(refuse(format!("ignored explicit argument {}", python_repr(&value))));
                        }
                    }
                    None => {
                        let wanted = count(&letters[start + 1..])?;
                        taken.push((index.expect("an option"), tokens[start + 1..start + 1 + wanted].to_vec()));
                        stop = start + 1 + wanted;
                        break;
                    }
                }
            }
            for (index, values) in taken {
                let option = &self.options[index];
                if option.strings.iter().any(|s| s == "--help") {
                    return Ok(vec!["--help".to_string()]);
                }
                let mut converted = Vec::new();
                for value in &values {
                    converted
                        .push(Self::value(option.kind, value).map_err(|e| format!("argument {}: {e}", option.name()))?);
                }
                given[index] = Some(converted);
            }
            start = stop;
        }
        let end = take_positionals(start, &mut waiting, &mut positionals);
        extras.extend(tokens[end..].iter().cloned());
        if !extras.is_empty() {
            return Err(format!("unrecognized arguments: {}", extras.join(" ")));
        }

        let mut out = Vec::new();
        for (option, values) in self.options.iter().zip(given) {
            let (Some(name), Some(values)) = (&option.emit, values) else { continue };
            match option.nargs {
                Nargs::Flag => out.push(format!("--{name}")),
                _ => out.extend(values.iter().map(|value| format!("--{name}={value}"))),
            }
        }
        if !positionals.is_empty() {
            out.push("--".to_string());
            out.extend(positionals);
        }
        Ok(out)
    }
}

/// Python `repr` of a text, as argparse quotes a value in its messages.
fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}
