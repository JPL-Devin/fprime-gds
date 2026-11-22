//! What is printed: received events and channel samples, and the definitions of the
//! dictionary. Text and JSON are the lines of `fprime-cli` (`get_str` and `json.dumps(get_dict())` of
//! the Python GDS), apart from what docs/design/deviations.md lists.

use fgds_core::clap::ArgMatches;
use fgds_core::python_times as times;
use fgds_core::serde_json::{json, Value};

/// One printed item.
pub struct Item {
    pub id: u64,
    pub name: String,
    text: String,
    json: String,
}

/// `json.dumps` of Python: `, ` and `: ` between items, text in ASCII, a float as `repr` writes it.
fn dumps(value: &Value) -> String {
    fgds_offline::value::Value::from_json(value).to_json()
}

/// An object of Python's `json.dumps` from keys and values already written.
fn object(fields: &[(&str, String)]) -> String {
    let fields: Vec<String> = fields.iter().map(|(key, value)| format!("{}: {value}", dumps(&json!(key)))).collect();
    format!("{{{}}}", fields.join(", "))
}

/// `str()` of an optional text: `None` when there is none.
fn or_none(text: Option<&str>) -> &str {
    text.unwrap_or("None")
}

impl Item {
    pub fn line(&self, json: bool) -> String {
        if json {
            self.json.clone()
        } else {
            self.text.clone()
        }
    }

    /// The name without its last part: `CdhCore.cmdDisp` of `CdhCore.cmdDisp.CMD_NO_OP`.
    fn component(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(component, _)| component)
    }

    /// A record of kind `fprime.event` or `fprime.channel`. `plain` holds the kind of the type of
    /// each channel that has no format string: its JSON shows the value, not a text.
    pub fn from_record(record: &Value, plain: &std::collections::HashMap<u64, String>) -> Item {
        let body = &record["body"];
        let time = &record["flight_time"];
        let field = |name: &str| time[name].as_u64().unwrap_or(0);
        let (readable, raw) = times(field("base"), field("context"), field("seconds"), field("microseconds"));
        let id = record["id"].as_u64().unwrap_or(0);
        let name = body["name"].as_str().unwrap_or("?").to_string();
        let text = body["display_text"].as_str().unwrap_or("");
        match body["severity"].as_str() {
            Some(severity) => event(id, name, severity, &readable, &raw, &body["args"], Some(text)),
            None => {
                let shown = match (&body["display_members"], plain.get(&id).map(String::as_str), &body["value"]) {
                    (Value::Null, Some("integer" | "float"), Value::String(number)) => number.clone(),
                    (Value::Null, Some(_), value) => dumps(value),
                    (Value::Null, None, _) => dumps(&json!(text)),
                    (members, _, _) => dumps(members),
                };
                channel(id, name, &readable, &raw, text, shown)
            }
        }
    }
}

fn event(id: u64, name: String, severity: &str, time: &str, raw: &str, args: &Value, text: Option<&str>) -> Item {
    let severity = format!("EventSeverity.{severity}");
    Item {
        id,
        text: fgds_core::python_event_line(time, &name, id, raw, &severity, text),
        json: object(&[
            ("time", dumps(&json!(time))),
            ("raw_time", dumps(&json!(raw))),
            ("name", dumps(&json!(name))),
            ("id", id.to_string()),
            ("severity", dumps(&json!(severity))),
            ("args", dumps(args)),
            ("display_text", dumps(&json!(text))),
        ]),
        name,
    }
}

/// `shown` is the display text as JSON writes it.
fn channel(id: u64, name: String, time: &str, raw: &str, text: &str, shown: String) -> Item {
    let json = object(&[
        ("time", dumps(&json!(time))),
        ("raw_time", dumps(&json!(raw))),
        ("name", dumps(&json!(name))),
        ("id", id.to_string()),
        ("display_text", shown),
    ]);
    Item { id, text: format!("{time}: {name} ({id}) {raw} {text}"), json, name }
}

/// The kind of the type of each channel without a format string (`integer`, `float`, ...).
pub fn plain_channels(dictionary: &Value) -> std::collections::HashMap<u64, String> {
    list(dictionary, "telemetryChannels")
        .filter(|c| c["format"].as_str().is_none())
        .map(|c| {
            let alias = list(dictionary, "typeDefinitions")
                .find(|t| text(t, "kind") == "alias" && t["qualifiedName"] == c["type"]["name"]);
            let of = alias.map_or(&c["type"], |a| &a["underlyingType"]);
            (c["id"].as_u64().unwrap_or(0), text(of, "kind").to_string())
        })
        .collect()
}

fn sorted(mut items: Vec<Item>) -> Vec<Item> {
    items.sort_by_key(|item| item.id);
    items
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

fn list<'a>(dictionary: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    dictionary[key].as_array().into_iter().flatten()
}

/// The event definitions of a dictionary, sorted by identifier. As in `fprime-cli`,
/// each prints like an event received at time zero whose text is its description
/// (`null` in JSON when the dictionary gives none).
pub fn events(dictionary: &Value) -> Vec<Item> {
    let (time, raw) = times(0, 0, 0, 0);
    let events = list(dictionary, "events").map(|e| {
        let id = e["id"].as_u64().unwrap_or(0);
        event(id, text(e, "name").into(), text(e, "severity"), &time, &raw, &Value::Null, e["annotation"].as_str())
    });
    sorted(events.collect())
}

/// The channel definitions of a dictionary, sorted by identifier.
pub fn channels(dictionary: &Value) -> Vec<Item> {
    let (time, raw) = times(0, 0, 0, 0);
    let channels = list(dictionary, "telemetryChannels").map(|c| {
        let description = c["annotation"].as_str();
        let shown = dumps(&json!(description));
        channel(c["id"].as_u64().unwrap_or(0), text(c, "name").into(), &time, &raw, or_none(description), shown)
    });
    sorted(channels.collect())
}

/// The command definitions of a dictionary, sorted by opcode.
pub fn commands(dictionary: &Value) -> Vec<Item> {
    let commands = list(dictionary, "commands").map(|c| {
        let (name, opcode) = (text(c, "name").to_string(), c["opcode"].as_u64().unwrap_or(0));
        let (component, mnemonic) = name.rsplit_once('.').unwrap_or(("", &name));
        let params: Vec<&Value> = list(c, "formalParams").collect();
        let mut lines = format!("{name} ({opcode}) | Takes {} arguments.\n", params.len());
        if !text(c, "annotation").is_empty() {
            lines += &format!("Description: {}\n", text(c, "annotation"));
        }
        let mut args = Vec::new();
        for p in params {
            let arg = text(p, "name");
            // As the Python GDS does: an alias is named by the type it stands for, a
            // string type with its size.
            let alias = list(dictionary, "typeDefinitions")
                .find(|t| text(t, "kind") == "alias" && t["qualifiedName"] == p["type"]["name"]);
            let of = alias.map_or(&p["type"], |a| &a["underlyingType"]);
            let kind = match (text(of, "kind"), of["size"].as_u64()) {
                ("string", Some(size)) => format!("String_{size}"),
                ("integer" | "float", _) => format!("{}Type", text(of, "name")),
                ("bool", _) => "BoolType".to_string(),
                _ => text(of, "name").to_string(),
            };
            let description = text(p, "annotation");
            let shown = if description.is_empty() { "--no description--" } else { description };
            lines += &format!("\t{arg} ({kind}): {shown}\n");
            args.push(json!({"name": arg, "description": description, "type": kind}));
        }
        let json = json!({
            "args": args, "comp_name": component, "description": text(c, "annotation"), "full_name": name,
            "id": opcode, "mnemonic": mnemonic, "name": mnemonic, "op_code": opcode,
        });
        Item { id: opcode, text: lines, json: dumps(&json), name }
    });
    sorted(commands.collect())
}

/// `difflib.get_close_matches(name, names, n=3)` of Python: the names whose ratio to `name` is at
/// least 0.6, best first; equal ratios in descending order of the name.
pub fn similar(name: &str, commands: &[Item]) -> Vec<String> {
    let wanted: Vec<char> = name.chars().collect();
    let mut scored: Vec<(f64, &str)> = commands
        .iter()
        .map(|c| (ratio(&c.name.chars().collect::<Vec<_>>(), &wanted), c.name.as_str()))
        .filter(|(ratio, _)| *ratio >= 0.6)
        .collect();
    scored.sort_by(|x, y| y.0.total_cmp(&x.0).then(y.1.cmp(x.1)));
    scored.into_iter().take(3).map(|(_, name)| name.to_string()).collect()
}

/// `SequenceMatcher(None, a, b).ratio()` for texts under 200 characters (no automatic junk).
fn ratio(a: &[char], b: &[char]) -> f64 {
    fn matched(a: &[char], b: &[char]) -> usize {
        // The longest common block, the first in `a` then in `b`; then the same on each side of it.
        let (mut best_i, mut best_j, mut best) = (0, 0, 0);
        let mut lengths = vec![0usize; b.len() + 1];
        for (i, x) in a.iter().enumerate() {
            let mut next = vec![0usize; b.len() + 1];
            for (j, y) in b.iter().enumerate() {
                if x == y {
                    let length = lengths[j] + 1;
                    next[j + 1] = length;
                    if length > best {
                        (best_i, best_j, best) = (i + 1 - length, j + 1 - length, length);
                    }
                }
            }
            lengths = next;
        }
        if best == 0 {
            return 0;
        }
        best + matched(&a[..best_i], &b[..best_j]) + matched(&a[best_i + best..], &b[best_j + best..])
    }
    match a.len() + b.len() {
        0 => 1.0,
        total => 2.0 * matched(a, b) as f64 / total as f64,
    }
}

/// The filters of `fprime-cli`: identifiers, components, and a string searched in the
/// line as it is printed. An item passes when it passes each filter that is given.
#[derive(Default)]
pub struct Filter {
    pub ids: Vec<i128>,
    pub components: Vec<String>,
    pub search: Option<String>,
}

impl Filter {
    /// The error is the one of argparse for `type=int`: decimal digits only.
    pub fn from(args: &ArgMatches) -> Result<Filter, String> {
        let mut ids = Vec::new();
        for id in args.get_many::<String>("ids").into_iter().flatten() {
            match id.trim().parse::<i128>() {
                Ok(id) => ids.push(id),
                Err(_) => return Err(format!("argument -i/--ids: invalid int value: '{id}'")),
            }
        }
        Ok(Filter {
            ids,
            components: args.get_many::<String>("components").map(|c| c.cloned().collect()).unwrap_or_default(),
            search: args.get_one::<String>("search").cloned(),
        })
    }

    pub fn accepts(&self, item: &Item, json: bool) -> bool {
        (self.ids.is_empty() || self.ids.contains(&i128::from(item.id)))
            && (self.components.is_empty() || self.components.iter().any(|c| c == item.component()))
            && self.search.as_ref().is_none_or(|s| item.line(json).contains(s.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary() -> Value {
        json!({
            "events": [
                {"name": "a.b.Second", "severity": "WARNING_HI", "id": 2, "format": "x {}", "annotation": "Second event"},
                {"name": "a.c.First", "severity": "ACTIVITY_LO", "id": 1, "format": "y"},
            ],
            "telemetryChannels": [{"name": "a.b.Count", "id": 7, "annotation": "A count"}],
            "commands": [
                {"name": "a.b.GO", "opcode": 16, "annotation": "Go", "formalParams": [
                    {"name": "speed", "type": {"name": "U8", "kind": "integer"}, "annotation": "How fast"},
                    {"name": "mode", "type": {"name": "a.Mode", "kind": "qualifiedIdentifier"}},
                ]},
                {"name": "a.b.STOP", "opcode": 3, "formalParams": []},
            ],
        })
    }

    #[test]
    fn a_received_event_prints_like_fprime_cli() {
        let item = Item::from_record(
            &json!({
                "seq": "4", "kind": "fprime.event", "id": 16777217,
                "flight_time": {"base": 2, "context": 0, "seconds": 1795275108, "microseconds": 643},
                "body": {"name": "CdhCore.cmdDisp.OpCodeCompleted", "severity": "COMMAND", "args": {"Opcode": 1},
                         "display_text": "Opcode 0x1 completed"},
            }),
            &Default::default(),
        );
        assert_eq!(
            item.line(false),
            "2026-11-21T15:31:48.000643: CdhCore.cmdDisp.OpCodeCompleted (16777217) (2(0)-1795275108:643) EventSeverity.COMMAND : Opcode 0x1 completed"
        );
        assert_eq!(
            item.line(true),
            r#"{"time": "2026-11-21T15:31:48.000643", "raw_time": "(2(0)-1795275108:643)", "name": "CdhCore.cmdDisp.OpCodeCompleted", "id": 16777217, "severity": "EventSeverity.COMMAND", "args": {"Opcode": 1}, "display_text": "Opcode 0x1 completed"}"#
        );
    }

    #[test]
    fn a_received_channel_sample_prints_like_fprime_cli() {
        let sample = |body: Value| {
            json!({"seq": "5", "kind": "fprime.channel", "id": 7, "body": body,
                   "flight_time": {"base": 1, "context": 3, "seconds": 12, "microseconds": 5}})
        };
        let start = r#"{"time": "TB_PROC_TIME: 12.000005s, context=3", "raw_time": "(1(3)-12:5)", "name": "a.b.Count", "id": 7, "display_text": "#;
        let item = Item::from_record(
            &sample(json!({"name": "a.b.Count", "value": 9, "display_text": "9 m"})),
            &Default::default(),
        );
        assert_eq!(item.line(false), "TB_PROC_TIME: 12.000005s, context=3: a.b.Count (7) (1(3)-12:5) 9 m");
        assert_eq!(item.line(true), format!(r#"{start}"9 m"}}"#));
        // Without a format string the value itself is shown; a 64-bit integer is a text in the record.
        for (kind, mut body, shown) in [
            ("integer", json!({"value": 9, "display_text": "9"}), "9"),
            (
                "integer",
                json!({"value": "18446744073709551615", "display_text": "18446744073709551615"}),
                "18446744073709551615",
            ),
            ("float", json!({"value": "Infinity", "display_text": "inf"}), "Infinity"),
            ("string", json!({"value": "a b", "display_text": "a b"}), r#""a b""#),
            (
                "array",
                json!({"value": [12, 1], "display_text": "['12', '1']", "display_members": ["12", "1"]}),
                r#"["12", "1"]"#,
            ),
        ] {
            body["name"] = json!("a.b.Count");
            let item = Item::from_record(&sample(body), &[(7, kind.to_string())].into());
            assert_eq!(item.line(true), format!("{start}{shown}}}"));
        }
    }

    #[test]
    fn definitions_are_sorted_by_identifier() {
        let events = events(&dictionary());
        assert_eq!(events.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            events[1].line(false),
            "TB_NONE: 0.000000s, context=0: a.b.Second (2) (0(0)-0:0) EventSeverity.WARNING_HI : Second event"
        );
        assert_eq!(
            channels(&dictionary())[0].line(false),
            "TB_NONE: 0.000000s, context=0: a.b.Count (7) (0(0)-0:0) A count"
        );
        let commands = commands(&dictionary());
        assert_eq!(commands[0].name, "a.b.STOP");
        assert_eq!(
            commands[1].line(false),
            "a.b.GO (16) | Takes 2 arguments.\nDescription: Go\n\tspeed (U8Type): How fast\n\tmode (a.Mode): --no description--\n"
        );
    }

    #[test]
    fn filters_combine() {
        let events = events(&dictionary());
        let accepted =
            |filter: Filter| events.iter().filter(|e| filter.accepts(e, false)).map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(accepted(Filter::default()), [1, 2]);
        assert_eq!(accepted(Filter { ids: vec![2, 9], ..Default::default() }), [2]);
        assert_eq!(accepted(Filter { components: vec!["a.c".into()], ..Default::default() }), [1]);
        assert_eq!(accepted(Filter { components: vec!["a".into()], ..Default::default() }), [0u64; 0]);
        assert_eq!(accepted(Filter { search: Some("WARNING".into()), ..Default::default() }), [2]);
        assert_eq!(accepted(Filter { ids: vec![1], search: Some("WARNING".into()), ..Default::default() }), [0u64; 0]);
    }

    #[test]
    fn close_command_names_are_suggested() {
        let commands = commands(&dictionary());
        assert_eq!(similar("a.b.STOPP", &commands), ["a.b.STOP", "a.b.GO"]);
        assert!(similar("unrelated.name.entirely", &commands).is_empty());
    }
}
