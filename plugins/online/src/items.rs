//! What is printed: received events and channel samples, and the definitions of the
//! dictionary. The text form follows `fprime-cli`; the JSON form has its field names.

use fgds_core::clap::ArgMatches;
use fgds_core::serde_json::{json, Value};

/// One printed item.
pub struct Item {
    pub id: u64,
    pub name: String,
    text: String,
    json: Value,
}

impl Item {
    pub fn line(&self, json: bool) -> String {
        if json {
            self.json.to_string()
        } else {
            self.text.clone()
        }
    }

    /// The name without its last part: `CdhCore.cmdDisp` of `CdhCore.cmdDisp.CMD_NO_OP`.
    fn component(&self) -> &str {
        self.name.rsplit_once('.').map_or("", |(component, _)| component)
    }

    /// A record of kind `fprime.event` or `fprime.channel`.
    pub fn from_record(record: &Value) -> Item {
        let body = &record["body"];
        let time = &record["flight_time"];
        let field = |name: &str| time[name].as_u64().unwrap_or(0);
        let (readable, raw) = times(field("base"), field("context"), field("seconds"), field("microseconds"));
        let id = record["id"].as_u64().unwrap_or(0);
        let name = body["name"].as_str().unwrap_or("?").to_string();
        let text = Some(body["display_text"].as_str().unwrap_or(""));
        match body["severity"].as_str() {
            Some(severity) => event(id, name, severity, &readable, &raw, body["args"].clone(), text),
            None => channel(id, name, &readable, &raw, Some(body["value"].clone()), text),
        }
    }
}

fn event(id: u64, name: String, severity: &str, time: &str, raw: &str, args: Value, text: Option<&str>) -> Item {
    Item {
        id,
        text: format!("{time}: {name} ({id}) {raw} {severity} : {}", text.unwrap_or("")),
        json: json!({
            "time": time, "raw_time": raw, "name": name, "id": id,
            "severity": severity, "args": args, "display_text": text,
        }),
        name,
    }
}

fn channel(id: u64, name: String, time: &str, raw: &str, value: Option<Value>, text: Option<&str>) -> Item {
    let mut json = json!({"time": time, "raw_time": raw, "name": name, "id": id});
    if let Some(value) = value {
        json["value"] = value;
    }
    json["display_text"] = json!(text);
    Item { id, text: format!("{time}: {name} ({id}) {raw} {}", text.unwrap_or("")), json, name }
}

/// A flight time, readable and raw. The raw form is the one of the Python GDS; the
/// readable one is UTC for workstation time (the Python GDS prints local time).
fn times(base: u64, context: u64, seconds: u64, microseconds: u64) -> (String, String) {
    const WORKSTATION: u64 = 2;
    let raw = format!("({base}({context})-{seconds}:{microseconds})");
    let date = (base == WORKSTATION).then(|| time::OffsetDateTime::from_unix_timestamp(seconds as i64).ok()).flatten();
    let readable = match date {
        Some(t) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{microseconds:06}+00:00",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        None => {
            let base = match base {
                0 => "TB_NONE".to_string(),
                1 => "TB_PROC_TIME".to_string(),
                WORKSTATION => "TB_WORKSTATION_TIME".to_string(),
                3 => "TB_SC_TIME".to_string(),
                0xffff => "TB_DONT_CARE".to_string(),
                other => format!("TB_{other}"),
            };
            format!("{base}: {seconds}.{microseconds:06}s, context={context}")
        }
    };
    (readable, raw)
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
        event(id, text(e, "name").into(), text(e, "severity"), &time, &raw, Value::Null, e["annotation"].as_str())
    });
    sorted(events.collect())
}

/// The channel definitions of a dictionary, sorted by identifier.
pub fn channels(dictionary: &Value) -> Vec<Item> {
    let (time, raw) = times(0, 0, 0, 0);
    let channels = list(dictionary, "telemetryChannels").map(|c| {
        channel(c["id"].as_u64().unwrap_or(0), text(c, "name").into(), &time, &raw, None, c["annotation"].as_str())
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
        Item { id: opcode, text: lines, json, name }
    });
    sorted(commands.collect())
}

/// Up to three command names close to `name`, closest first.
pub fn similar(name: &str, commands: &[Item]) -> Vec<String> {
    let wanted = name.to_ascii_lowercase();
    let mut scored: Vec<(usize, &str)> = commands
        .iter()
        .map(|c| (distance(&wanted, &c.name.to_ascii_lowercase()), c.name.as_str()))
        .filter(|(d, candidate)| *d * 5 <= wanted.len().max(candidate.len()) * 2)
        .collect();
    scored.sort();
    scored.into_iter().take(3).map(|(_, name)| name.to_string()).collect()
}

/// Edit distance.
fn distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let substitution = diagonal + usize::from(x != y);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(diagonal + 1);
        }
    }
    row[b.len()]
}

/// The filters of `fprime-cli`: identifiers, components, and a string searched in the
/// line as it is printed. An item passes when it passes each filter that is given.
#[derive(Default)]
pub struct Filter {
    pub ids: Vec<u64>,
    pub components: Vec<String>,
    pub search: Option<String>,
}

impl Filter {
    pub fn from(args: &ArgMatches) -> Filter {
        Filter {
            ids: args.get_many::<u64>("ids").map(|ids| ids.copied().collect()).unwrap_or_default(),
            components: args.get_many::<String>("components").map(|c| c.cloned().collect()).unwrap_or_default(),
            search: args.get_one::<String>("search").cloned(),
        }
    }

    pub fn accepts(&self, item: &Item, json: bool) -> bool {
        (self.ids.is_empty() || self.ids.contains(&item.id))
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
        let item = Item::from_record(&json!({
            "seq": "4", "kind": "fprime.event", "id": 16777217,
            "flight_time": {"base": 2, "context": 0, "seconds": 1795275108, "microseconds": 643},
            "body": {"name": "CdhCore.cmdDisp.OpCodeCompleted", "severity": "COMMAND", "args": {"Opcode": 1},
                     "display_text": "Opcode 0x1 completed"},
        }));
        assert_eq!(
            item.line(false),
            "2026-11-21T15:31:48.000643+00:00: CdhCore.cmdDisp.OpCodeCompleted (16777217) (2(0)-1795275108:643) COMMAND : Opcode 0x1 completed"
        );
        assert_eq!(
            item.line(true),
            r#"{"time":"2026-11-21T15:31:48.000643+00:00","raw_time":"(2(0)-1795275108:643)","name":"CdhCore.cmdDisp.OpCodeCompleted","id":16777217,"severity":"COMMAND","args":{"Opcode":1},"display_text":"Opcode 0x1 completed"}"#
        );
    }

    #[test]
    fn a_received_channel_sample_prints_like_fprime_cli() {
        let item = Item::from_record(&json!({
            "seq": "5", "kind": "fprime.channel", "id": 7,
            "flight_time": {"base": 1, "context": 3, "seconds": 12, "microseconds": 5},
            "body": {"name": "a.b.Count", "value": 9, "display_text": "9 m"},
        }));
        assert_eq!(item.line(false), "TB_PROC_TIME: 12.000005s, context=3: a.b.Count (7) (1(3)-12:5) 9 m");
        assert_eq!(
            item.line(true),
            r#"{"time":"TB_PROC_TIME: 12.000005s, context=3","raw_time":"(1(3)-12:5)","name":"a.b.Count","id":7,"value":9,"display_text":"9 m"}"#
        );
    }

    #[test]
    fn definitions_are_sorted_by_identifier() {
        let events = events(&dictionary());
        assert_eq!(events.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(
            events[1].line(false),
            "TB_NONE: 0.000000s, context=0: a.b.Second (2) (0(0)-0:0) WARNING_HI : Second event"
        );
        assert_eq!(
            channels(&dictionary())[0].line(false),
            "TB_NONE: 0.000000s, context=0: a.b.Count (7) (0(0)-0:0) A count"
        );
        let commands = commands(&dictionary());
        assert_eq!(commands[0].name, "a.b.STOP");
        assert_eq!(
            commands[1].line(false),
            "a.b.GO (16) | Takes 2 arguments.\nDescription: Go\n\tspeed (U8): How fast\n\tmode (a.Mode): --no description--\n"
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
        assert_eq!(similar("a.b.STOPP", &commands), ["a.b.STOP", "a.b.GO"][..1]);
        assert!(similar("unrelated.name.entirely", &commands).is_empty());
    }
}
