//! The lines the prompt prints: records of the stream, and the definitions of the dictionary.

use crate::filter::found;
use crate::line::List;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use fgds_core::serde_json::Value;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn list<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

/// `text` as a terminal may be given it. Everything `fgds repl` and `fgds tui` print that comes
/// from flight, from the dictionary or from what the operator typed goes through here, once.
///
/// - C0, DEL and C1 are shown: `\x1b`, `\r`, `\n`, `\t`, the others as `\xNN`.
/// - A backslash is doubled, so what is shown is read back in one way only.
/// - Line and paragraph separators, the marks and controls of text direction, and the characters
///   of no width are shown as `\u{XXXX}`. Other valid text is unchanged.
pub fn visible(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => shown.push_str("\\\\"),
            '\r' => shown.push_str("\\r"),
            '\n' => shown.push_str("\\n"),
            '\t' => shown.push_str("\\t"),
            c if c.is_control() => shown.push_str(&format!("\\x{:02x}", u32::from(c))),
            '\u{2028}'
            | '\u{2029}'
            | '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200b}'..='\u{200d}'
            | '\u{2060}'
            | '\u{feff}' => {
                shown.push_str(&format!("\\u{{{:04X}}}", u32::from(c)));
            }
            c => shown.push(c),
        }
    }
    shown
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn flight_time(record: &Value) -> String {
    let field = |name: &str| record["flight_time"][name].as_u64().unwrap_or(0);
    fgds_core::python_times(field("base"), field("context"), field("seconds"), field("microseconds")).0
}

/// An event as a terminal may be given it, with its severity in colour when `colour` is set.
pub fn event(record: &Value, colour: bool) -> String {
    let body = &record["body"];
    let severity = text(body, "severity");
    let code = match severity {
        "FATAL" | "WARNING_HI" => "31",
        "WARNING_LO" => "33",
        "COMMAND" => "32",
        "ACTIVITY_HI" => "36",
        _ => "0",
    };
    let severity = visible(severity);
    let shown = if colour { format!("\x1b[{code}m{severity}\x1b[0m") } else { severity };
    let rest = visible(&format!("{}: {}", text(body, "name"), text(body, "display_text")));
    format!("{} EVENT {shown} {rest}", flight_time(record))
}

/// Not yet `visible`, as `command` and the lists below: `View::line` and `Session::handle` see to it.
pub fn channel(record: &Value) -> String {
    let body = &record["body"];
    format!("{} CHAN  {} = {}", flight_time(record), text(body, "name"), text(body, "display_text"))
}

/// A command record: what was sent, or why it was not.
pub fn command(record: &Value) -> String {
    let body = &record["body"];
    let outcome = match text(body, "outcome") {
        "sent" => {
            let packet = hex(&STANDARD.decode(text(record, "raw")).unwrap_or_default());
            format!("sent, opcode {}, packet {packet}", body["opcode"].as_u64().unwrap_or(0))
        }
        other => format!("{other}: {}", text(body, "reason")),
    };
    let time = text(record, "ground_time");
    format!("{time} CMD   {} {} by {}: {outcome}", text(body, "name"), body["args"], text(body, "client"))
}

fn type_name(ty: &Value) -> String {
    match (text(ty, "kind"), ty["size"].as_u64()) {
        ("string", Some(size)) => format!("string[{size}]"),
        _ => text(ty, "name").to_string(),
    }
}

fn parameters(definition: &Value) -> String {
    let parameters: Vec<String> =
        list(definition, "formalParams").map(|p| format!("{}: {}", text(p, "name"), type_name(&p["type"]))).collect();
    parameters.join(", ")
}

fn key(list: List) -> (&'static str, &'static str) {
    match list {
        List::Commands => ("commands", "opcode"),
        List::Events => ("events", "id"),
        List::Channels => ("telemetryChannels", "id"),
    }
}

/// One line per definition whose name fits `pattern`, sorted by name.
pub fn definitions(dictionary: &Value, which: List, pattern: Option<&str>) -> Vec<String> {
    let (key, id) = key(which);
    let mut lines: Vec<String> = list(dictionary, key)
        .filter(|d| pattern.is_none_or(|p| found(p, text(d, "name"))))
        .map(|d| {
            let (name, id) = (text(d, "name"), d[id].as_u64().unwrap_or(0));
            match which {
                List::Commands => format!("{name} (0x{id:x}) ({})", parameters(d)),
                List::Events => format!("{name} (0x{id:x}) {}", text(d, "severity")),
                List::Channels => format!("{name} (0x{id:x}) {}", type_name(&d["type"])),
            }
        })
        .collect();
    lines.sort();
    lines
}

/// The parameter names of the command called `name`; `None` when the dictionary has no such command.
pub fn command_parameters(dictionary: &Value, name: &str) -> Option<Vec<String>> {
    let command = list(dictionary, "commands").find(|c| c["name"] == name)?;
    Some(list(command, "formalParams").map(|p| text(p, "name").to_string()).collect())
}

/// How the command called `name` is typed.
pub fn usage(dictionary: &Value, name: &str) -> String {
    let parameters = command_parameters(dictionary, name).unwrap_or_default();
    let parameters: Vec<String> = parameters.iter().map(|p| format!(" <{p}>")).collect();
    format!("usage: cmd {name}{}", parameters.concat())
}

/// `help <name>`: the command, event or channel of that name.
pub fn help(dictionary: &Value, name: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for which in [List::Commands, List::Events, List::Channels] {
        let (key, id) = key(which);
        let Some(d) = list(dictionary, key).find(|d| d["name"] == name) else { continue };
        let id = d[id].as_u64().unwrap_or(0);
        match which {
            List::Commands => {
                lines.push(format!("command {name}, opcode {id} (0x{id:x})"));
                lines.extend(list(d, "formalParams").map(|p| {
                    format!("  {}: {}  {}", text(p, "name"), type_name(&p["type"]), text(p, "annotation"))
                        .trim_end()
                        .to_string()
                }));
                lines.push(usage(dictionary, name));
            }
            List::Events => {
                lines.push(format!("event {name}, id {id} (0x{id:x}), severity {}", text(d, "severity")));
                lines.push(format!("  format: {}", text(d, "format")));
                lines.extend(
                    list(d, "formalParams").map(|p| format!("  {}: {}", text(p, "name"), type_name(&p["type"]))),
                );
            }
            List::Channels => lines.push(format!("channel {name}, id {id} (0x{id:x}), type {}", type_name(&d["type"]))),
        }
        if !text(d, "annotation").is_empty() {
            lines.push(format!("  {}", text(d, "annotation")));
        }
    }
    if lines.is_empty() {
        lines.push(format!("no command, event or channel is called {name} (try `cmds {name}`)"));
    }
    lines
}

pub const HELP: &str =
    "  cmd <name> [argument ...]       send a command; arguments in order, or all as <parameter>=<value>
  raw-uplink <hexadecimal bytes>  send bytes as one packet
  cmds|events|channels [pattern]  list the dictionary (also: dict commands|events|channels [pattern])
  help [name]                     this text, or one command, event or channel
  mute events|channels|<pattern>  stop printing a stream, or the names that match
  unmute events|channels|<pattern>
  subscribe <pattern>             print only the names that match a subscription
  unsubscribe <pattern>
  filters                         list the mutes and subscriptions
  clear-filters                   print everything again
  quit                            leave (also: exit, Ctrl-D)
Patterns: * is any text, ? one character (mute Ref.systemResources.CPU_*). In a list, a pattern
without * or ? is looked for anywhere in the name. Quote an argument that holds spaces. Tab completes.";

#[cfg(test)]
mod tests {
    use super::*;
    use fgds_core::serde_json::json;

    #[test]
    fn what_a_terminal_would_obey_is_shown_not_written() {
        assert_eq!(
            visible("hi\u{1b}]0;T\u{7}\u{1b}[2J\r\n\t\u{7f}\u{9b}"),
            "hi\\x1b]0;T\\x07\\x1b[2J\\r\\n\\t\\x7f\\x9b"
        );
        assert_eq!(visible("a\\x1b é漢🚀"), "a\\\\x1b é漢🚀");
        let hidden =
            "\u{2028}\u{2029}\u{061c}\u{200e}\u{200f}\u{202a}\u{202e}\u{2066}\u{2069}\u{200b}\u{200d}\u{2060}\u{feff}";
        assert_eq!(
            visible(hidden),
            "\\u{2028}\\u{2029}\\u{061C}\\u{200E}\\u{200F}\\u{202A}\\u{202E}\\u{2066}\\u{2069}\\u{200B}\\u{200D}\\u{2060}\\u{FEFF}"
        );
        let record =
            json!({"body": {"severity": "ACTIVITY_HI", "name": "A.b\u{1b}[2J", "display_text": "x\u{1b}]0;T\u{7}\\"}});
        assert!(
            event(&record, false).ends_with("EVENT ACTIVITY_HI A.b\\x1b[2J: x\\x1b]0;T\\x07\\\\"),
            "{}",
            event(&record, false)
        );
        // With colour, the only escape sequences are those of the severity.
        let coloured = event(&record, true);
        assert_eq!(coloured.matches('\u{1b}').count(), 2, "{coloured:?}");
        assert!(coloured.contains("\u{1b}[36mACTIVITY_HI\u{1b}[0m A.b\\x1b[2J"), "{coloured:?}");
    }

    fn dictionary() -> Value {
        json!({
            "commands": [
                {"name": "B.c.SET", "opcode": 17, "annotation": "Set it", "formalParams": [
                    {"name": "count", "type": {"name": "U32", "kind": "integer"}, "annotation": "How many"},
                    {"name": "label", "type": {"name": "string", "kind": "string", "size": 40}}]},
                {"name": "A.c.NO_OP", "opcode": 16, "formalParams": []}],
            "events": [{"name": "A.c.Done", "id": 2, "severity": "ACTIVITY_HI", "format": "Done {}",
                "formalParams": [{"name": "count", "type": {"name": "U32", "kind": "integer"}}]}],
            "telemetryChannels": [{"name": "A.c.Count", "id": 3, "type": {"name": "U32", "kind": "integer"}}],
        })
    }

    #[test]
    fn lists_are_sorted_and_filtered() {
        let d = dictionary();
        assert_eq!(
            definitions(&d, List::Commands, None),
            ["A.c.NO_OP (0x10) ()", "B.c.SET (0x11) (count: U32, label: string[40])"]
        );
        assert_eq!(definitions(&d, List::Commands, Some("set")), ["B.c.SET (0x11) (count: U32, label: string[40])"]);
        assert_eq!(definitions(&d, List::Events, Some("A.*")), ["A.c.Done (0x2) ACTIVITY_HI"]);
        assert_eq!(definitions(&d, List::Channels, None), ["A.c.Count (0x3) U32"]);
        assert!(definitions(&d, List::Channels, Some("zz")).is_empty());
    }

    #[test]
    fn help_of_a_name() {
        let d = dictionary();
        assert_eq!(
            help(&d, "B.c.SET"),
            [
                "command B.c.SET, opcode 17 (0x11)",
                "  count: U32  How many",
                "  label: string[40]",
                "usage: cmd B.c.SET <count> <label>",
                "  Set it"
            ]
        );
        assert_eq!(help(&d, "A.c.Done")[0], "event A.c.Done, id 2 (0x2), severity ACTIVITY_HI");
        assert_eq!(help(&d, "A.c.Count"), ["channel A.c.Count, id 3 (0x3), type U32"]);
        assert!(help(&d, "A.c.None")[0].starts_with("no command, event or channel"));
        assert_eq!(command_parameters(&d, "B.c.SET").unwrap(), ["count", "label"]);
        assert!(command_parameters(&d, "A.c.Done").is_none());
    }

    #[test]
    fn records_are_one_line() {
        let time = json!({"base": 2, "context": 0, "seconds": 0, "microseconds": 5});
        let record = json!({"flight_time": time, "body": {"name": "A.c.Done", "severity": "ACTIVITY_HI", "display_text": "Done 1"}});
        assert_eq!(event(&record, false), "1970-01-01T00:00:00.000005 EVENT ACTIVITY_HI A.c.Done: Done 1");
        assert!(event(&record, true).contains("\x1b[36mACTIVITY_HI\x1b[0m"));
        let record = json!({"flight_time": time, "body": {"name": "A.c.Count", "display_text": "7"}});
        assert_eq!(channel(&record), "1970-01-01T00:00:00.000005 CHAN  A.c.Count = 7");
        let record = json!({"ground_time": "T", "raw": "AAABAAAA",
            "body": {"name": "A.c.NO_OP", "opcode": 16, "args": {}, "client": "x", "outcome": "sent"}});
        assert_eq!(command(&record), "T CMD   A.c.NO_OP {} by x: sent, opcode 16, packet 000001000000");
        let record = json!({"ground_time": "T", "raw": "",
            "body": {"name": "A.c.NO_OP", "args": ["1"], "client": "x", "outcome": "rejected", "reason": "takes 0"}});
        assert_eq!(command(&record), "T CMD   A.c.NO_OP [\"1\"] by x: rejected: takes 0");
    }
}
