//! What is printed: received events and channel samples, and the definitions of the
//! dictionary. Text and JSON are the lines of `fprime-cli` (`get_str` and `json.dumps(get_dict())` of
//! the Python GDS), apart from what docs/design/deviations.md lists.

use fgds_core::clap::ArgMatches;
use fgds_core::python_times as times;
use fgds_core::serde_json::{json, Value};
use fgds_offline::value::Value as Typed;

/// One printed item.
pub struct Item {
    pub id: u64,
    pub name: String,
    /// As decoded: what `--search` reads.
    text: String,
    /// What is printed: `text` with its control characters written out (`fgds_core::visible`).
    shown: String,
    json: String,
}

fn dumps(value: &Value) -> String {
    // A line for a terminal: what is not ASCII is escaped, so that no character of the data ends the line.
    let mut out = String::new();
    for c in value.to_string().chars() {
        match c.is_ascii() {
            true => out.push(c),
            false => c.encode_utf16(&mut [0; 2]).iter().for_each(|unit| out.push_str(&format!("\\u{unit:04x}"))),
        }
    }
    out
}

/// An object from keys and values already written.
fn object(fields: &[(&str, String)]) -> String {
    let fields: Vec<String> = fields.iter().map(|(key, value)| format!("{}: {value}", dumps(&json!(key)))).collect();
    format!("{{{}}}", fields.join(", "))
}

/// `str()` of an optional text: `None` when there is none.
fn or_none(text: Option<&str>) -> &str {
    text.unwrap_or("None")
}

impl Item {
    /// An item of one line.
    fn new(id: u64, name: String, text: String, json: String) -> Item {
        let shown = fgds_core::visible(&text).into_owned();
        Item { id, name, text, shown, json }
    }

    /// What `--search` reads: the JSON, or the text as decoded, before its control characters are written out.
    pub fn line(&self, json: bool) -> String {
        if json {
            self.json.clone()
        } else {
            self.text.clone()
        }
    }

    /// What is printed: the JSON as it is, the text with its control characters written out.
    pub fn printed(&self, json: bool) -> &str {
        if json {
            &self.json
        } else {
            &self.shown
        }
    }

    /// The name without its last part: `CdhCore.cmdDisp` of `CdhCore.cmdDisp.CMD_NO_OP`.
    fn component(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(component, _)| component)
    }

    /// A record of kind `fprime.event` or `fprime.channel`. `plain` holds the kind of the type of
    /// each channel that has no format string: its JSON shows the value, not a text.
    /// `dictionary`, when given, tells which event arguments are numbers (see `typed`).
    pub fn from_record(
        record: &Value,
        plain: &std::collections::HashMap<u64, String>,
        dictionary: Option<&Value>,
    ) -> Item {
        let body = &record["body"];
        let time = &record["flight_time"];
        let field = |name: &str| time[name].as_u64().unwrap_or(0);
        let (readable, raw) = times(field("base"), field("context"), field("seconds"), field("microseconds"));
        let id = record["id"].as_u64().unwrap_or(0);
        let name = body["name"].as_str().unwrap_or("?").to_string();
        let text = body["display_text"].as_str().unwrap_or("");
        match body["severity"].as_str() {
            Some(severity) => {
                // The values in the order of the dictionary, without their names: the list `fprime-cli`
                // prints (`[]` for an event with no argument), so that a script reads one shape.
                let args = match &body["args"] {
                    Value::Object(named) => named.values().cloned().collect(),
                    Value::Array(values) => values.clone(),
                    _ => Vec::new(),
                };
                let types = dictionary.and_then(|d| list(d, "events").find(|e| e["id"] == record["id"]));
                let shown = match (types, dictionary) {
                    (Some(definition), Some(dictionary)) => {
                        let types = list(definition, "formalParams").map(|p| &p["type"]);
                        dumps(
                            &Typed::Array(args.iter().zip(types).map(|(a, t)| typed(a, t, dictionary)).collect())
                                .to_serde(),
                        )
                    }
                    _ => dumps(&Value::Array(args)),
                };
                event(id, name, severity, &readable, &raw, shown, Some(text))
            }
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

/// A value of a record as `json.dumps` of Python writes it: a 64-bit integer and a non-finite float, which
/// a record holds as text, are numbers again (`18446744073709551615`, `Infinity`, `NaN`), by the type the
/// dictionary gives.
fn typed(value: &Value, of: &Value, dictionary: &Value) -> Typed {
    let definition = || list(dictionary, "typeDefinitions").find(|t| t["qualifiedName"] == of["name"]);
    match (text(of, "kind"), value) {
        ("integer", Value::String(number)) => {
            number.parse::<i128>().map_or(Typed::from_json(value), |n| Typed::Int(n.into()))
        }
        ("float", Value::String(number)) => {
            number.replace("Infinity", "inf").parse::<f64>().map_or(Typed::from_json(value), Typed::Float)
        }
        ("qualifiedIdentifier", _) => match (definition(), value) {
            (Some(d), _) if text(d, "kind") == "alias" => typed(value, &d["underlyingType"], dictionary),
            (Some(d), Value::Array(items)) if text(d, "kind") == "array" => {
                Typed::Array(items.iter().map(|i| typed(i, &d["elementType"], dictionary)).collect())
            }
            (Some(d), Value::Object(members)) if text(d, "kind") == "struct" => Typed::Object(
                members
                    .iter()
                    .map(|(name, member)| {
                        let of = &d["members"][name]["type"];
                        let shown = match member {
                            // A member that is itself several values of its type.
                            Value::Array(items) if d["members"][name]["size"].is_u64() => {
                                Typed::Array(items.iter().map(|i| typed(i, of, dictionary)).collect())
                            }
                            _ => typed(member, of, dictionary),
                        };
                        (name.clone(), shown)
                    })
                    .collect(),
            ),
            _ => Typed::from_json(value),
        },
        _ => Typed::from_json(value),
    }
}

fn event(id: u64, name: String, severity: &str, time: &str, raw: &str, args: String, text: Option<&str>) -> Item {
    let severity = format!("EventSeverity.{severity}");
    let line = fgds_core::python_event_line(time, &name, id, raw, &severity, text);
    let json = object(&[
        ("time", dumps(&json!(time))),
        ("raw_time", dumps(&json!(raw))),
        ("name", dumps(&json!(name))),
        ("id", id.to_string()),
        ("severity", dumps(&json!(severity))),
        ("args", args),
        ("display_text", dumps(&json!(text))),
    ]);
    Item::new(id, name, line, json)
}

fn channel(id: u64, name: String, time: &str, raw: &str, text: &str, shown: String) -> Item {
    let json = object(&[
        ("time", dumps(&json!(time))),
        ("raw_time", dumps(&json!(raw))),
        ("name", dumps(&json!(name))),
        ("id", id.to_string()),
        ("display_text", shown),
    ]);
    Item::new(id, name.clone(), format!("{time}: {name} ({id}) {raw} {text}"), json)
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
        event(id, text(e, "name").into(), text(e, "severity"), &time, &raw, "null".into(), e["annotation"].as_str())
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
        // The same lines with the texts of the dictionary made visible, each on its line.
        let v = fgds_core::visible;
        let mut shown = format!("{} ({opcode}) | Takes {} arguments.\n", v(&name), params.len());
        if !text(c, "annotation").is_empty() {
            lines += &format!("Description: {}\n", text(c, "annotation"));
            shown += &format!("Description: {}\n", v(text(c, "annotation")));
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
            let (annotation, description) =
                (description, if description.is_empty() { "--no description--" } else { description });
            lines += &format!("\t{arg} ({kind}): {description}\n");
            shown += &format!("\t{} ({}): {}\n", v(arg), v(&kind), v(description));
            args.push(json!({"name": arg, "description": annotation, "type": kind}));
        }
        let json = json!({
            "args": args, "comp_name": component, "description": text(c, "annotation"), "full_name": name,
            "id": opcode, "mnemonic": mnemonic, "name": mnemonic, "op_code": opcode,
        });
        Item { id: opcode, text: lines, shown, json: dumps(&json), name }
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
    pub fn from(args: &ArgMatches) -> Filter {
        Filter {
            ids: args.get_many::<u32>("ids").map(|ids| ids.map(|id| i128::from(*id)).collect()).unwrap_or_default(),
            components: args.get_many::<String>("components").map(|c| c.cloned().collect()).unwrap_or_default(),
            search: args.get_one::<String>("search").cloned(),
        }
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
            None,
        );
        assert_eq!(
            item.line(false),
            "2026-11-21T15:31:48.000643: CdhCore.cmdDisp.OpCodeCompleted (16777217) (2(0)-1795275108:643) EventSeverity.COMMAND : Opcode 0x1 completed"
        );
        assert_eq!(
            item.line(true),
            r#"{"time": "2026-11-21T15:31:48.000643", "raw_time": "(2(0)-1795275108:643)", "name": "CdhCore.cmdDisp.OpCodeCompleted", "id": 16777217, "severity": "EventSeverity.COMMAND", "args": [1], "display_text": "Opcode 0x1 completed"}"#
        );
    }

    #[test]
    fn event_arguments_are_a_list_with_numbers_as_numbers() {
        let dictionary = json!({
            "typeDefinitions": [
                {"kind": "struct", "qualifiedName": "a.S", "members": {
                    "big": {"type": {"name": "U64", "kind": "integer"}},
                    "three": {"type": {"name": "F32", "kind": "float"}, "size": 3}}},
                {"kind": "array", "qualifiedName": "a.A", "elementType": {"name": "I64", "kind": "integer"}},
                {"kind": "alias", "qualifiedName": "a.T", "underlyingType": {"name": "F64", "kind": "float"}},
            ],
            "events": [
                {"id": 1, "formalParams": []},
                {"id": 2, "formalParams": [
                    {"name": "s", "type": {"name": "a.S", "kind": "qualifiedIdentifier"}},
                    {"name": "a", "type": {"name": "a.A", "kind": "qualifiedIdentifier"}},
                    {"name": "t", "type": {"name": "a.T", "kind": "qualifiedIdentifier"}},
                    {"name": "text", "type": {"name": "string", "kind": "string"}},
                ]},
            ],
        });
        let args = |id: u64, args: Value| {
            let record = json!({"id": id, "body": {"name": "a.b.E", "severity": "ACTIVITY_HI", "args": args}});
            let line = Item::from_record(&record, &Default::default(), Some(&dictionary)).line(true);
            let (_, rest) = line.split_once(r#""args": "#).unwrap();
            rest.split_once(r#", "display_text""#).unwrap().0.to_string()
        };
        assert_eq!(args(1, json!({})), "[]");
        assert_eq!(
            args(
                2,
                json!({"s": {"big": "18446744073709551615", "three": [1.5, "-Infinity", "NaN"]},
                       "a": ["-9223372036854775808", 2], "t": "Infinity", "text": "12"})
            ),
            r#"[{"big":18446744073709551615,"three":[1.5,"-Infinity","NaN"]},[-9223372036854775808,2],"Infinity","12"]"#
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
            None,
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
                json!({"value": [12, 1], "display_text": r#"["12","1"]"#, "display_members": ["12", "1"]}),
                r#"["12","1"]"#,
            ),
        ] {
            body["name"] = json!("a.b.Count");
            let item = Item::from_record(&sample(body), &[(7, kind.to_string())].into(), None);
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
