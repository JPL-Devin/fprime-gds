//! Text for a terminal or a log: control characters are shown, never sent.
//!
//! A string of the flight software or of the operator (an event argument, a file name) can hold
//! escape sequences that retitle or clear a terminal, and line ends that make one printed line
//! read as two. Every text output goes through [`visible`]; JSON output does not, it escapes by
//! its own rule and stays exact.

use std::borrow::Cow;

/// `text` with each character that a terminal acts on written out, and the backslash doubled, so
/// that the text printed reads back as the text given: `\n`, `\r`, `\t`, `\\`, `\x1b` for the
/// other ASCII control characters, `\u{9b}` for the rest (see `hidden`). Every other character
/// is kept.
pub fn visible(text: &str) -> Cow<'_, str> {
    shown(text, false)
}

/// As [`visible`] for a text of several lines put together by the program itself: its line ends
/// are kept.
pub fn visible_lines(text: &str) -> Cow<'_, str> {
    shown(text, true)
}

/// Control characters (C0, DEL, C1), the line and paragraph separators, the marks that change
/// the direction of the text, and the characters of no width.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{2028}' | '\u{2029}'
            | '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            | '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
}

fn shown(text: &str, lines: bool) -> Cow<'_, str> {
    let keep = |c: char| c != '\\' && (!hidden(c) || (lines && c == '\n'));
    if text.chars().all(keep) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            c if keep(c) => out.push(c),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
        }
    }
    Cow::Owned(out)
}

/// `println!` with the control characters of the whole line shown: one call, one line.
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => { println!("{}", $crate::visible(&format!($($arg)*))) };
}

/// `eprintln!` with the control characters of the whole line shown: one call, one line.
#[macro_export]
macro_rules! err {
    ($($arg:tt)*) => { eprintln!("{}", $crate::visible(&format!($($arg)*))) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_terminal_acts_on_is_written_out() {
        let event = "a\x1b]0;TITLE\x07b\x1b[2Jc\x1b[31mred\x1b[0m\r\nfake\tx\x7f\u{9b}2J\u{2028}\\n";
        let text = visible(event);
        assert_eq!(text, "a\\x1b]0;TITLE\\x07b\\x1b[2Jc\\x1b[31mred\\x1b[0m\\r\\nfake\\tx\\x7f\\u{9b}2J\\u{2028}\\\\n");
        assert_eq!(visible_lines("one\ntwo\r\x1b"), "one\ntwo\\r\\x1b");
        for c in (0..=0x9f)
            .chain([0x61c, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2028, 0x2029, 0x2060, 0xfeff])
            .chain(0x202a..=0x202e)
            .chain(0x2066..=0x2069)
            .filter_map(char::from_u32)
            .filter(|c| !(' '..='~').contains(c))
        {
            let text = visible(&format!("a{c}b")).into_owned();
            assert!(text.starts_with("a\\") && text.is_ascii(), "{:x}: {text}", c as u32);
        }
    }

    #[test]
    fn other_text_is_unchanged() {
        for text in ["", "plain ~ text", "é ü 日本 \u{1f680} \u{a0}\u{ad}", "<b>&amp;</b>"] {
            assert!(matches!(visible(text), Cow::Borrowed(same) if same == text));
        }
    }
}
