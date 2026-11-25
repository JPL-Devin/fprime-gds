//! What is printed: received events and channel samples, and the definitions of the dictionary. A text
//! line for a person; with `--json` the record of the API, or the definition of the dictionary, on one line.

use fgds_core::clap::ArgMatches;
use fgds_core::serde_json::Value;

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

/// JSON on one line for a terminal: what is not ASCII is escaped, so that no character of the data ends the line.
fn dumps(value: &Value) -> String {
    let mut out = String::new();
    for c in value.to_string().chars() {
        match c.is_ascii() {
            true => out.push(c),
            false => c.encode_utf16(&mut [0; 2]).iter().for_each(|unit| out.push_str(&format!("\\u{unit:04x}"))),
        }
    }
    out
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

    /// A record of kind `fprime.event` or `fprime.channel`.
    pub fn from_record(record: &Value) -> Item {
        let body = &record["body"];
        let id = record["id"].as_u64().unwrap_or(0);
        let name = body["name"].as_str().unwrap_or("?").to_string();
        let line = match body["severity"].is_string() {
            true => fgds_core::event_line(record),
            false => {
                let (time, raw) = fgds_core::flight_time_text(&record["flight_time"]);
                format!("{time}: {name} ({id}) {raw} {}", body["display_text"].as_str().unwrap_or(""))
            }
        };
        Item::new(id, name, line, dumps(record))
    }
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

/// The event definitions of a dictionary, sorted by identifier: name, identifier, severity and description;
/// in JSON, the definition as the dictionary has it.
pub fn events(dictionary: &Value) -> Vec<Item> {
    let events = list(dictionary, "events").map(|e| {
        let (id, name) = (e["id"].as_u64().unwrap_or(0), text(e, "name"));
        let line = format!("{name} ({id}) {} : {}", text(e, "severity"), text(e, "annotation"));
        Item::new(id, name.into(), line, dumps(e))
    });
    sorted(events.collect())
}

/// The channel definitions of a dictionary, sorted by identifier.
pub fn channels(dictionary: &Value) -> Vec<Item> {
    let channels = list(dictionary, "telemetryChannels").map(|c| {
        let (id, name) = (c["id"].as_u64().unwrap_or(0), text(c, "name"));
        Item::new(id, name.into(), format!("{name} ({id}) : {}", text(c, "annotation")), dumps(c))
    });
    sorted(channels.collect())
}

/// The command definitions of a dictionary, sorted by opcode: name, opcode, description and arguments;
/// in JSON, the definition as the dictionary has it.
pub fn commands(dictionary: &Value) -> Vec<Item> {
    let commands = list(dictionary, "commands").map(|c| {
        let (name, opcode) = (text(c, "name").to_string(), c["opcode"].as_u64().unwrap_or(0));
        let params: Vec<&Value> = list(c, "formalParams").collect();
        let mut lines = format!("{name} ({opcode}) | Takes {} arguments.\n", params.len());
        // The same lines with the texts of the dictionary made visible, each on its line.
        let v = fgds_core::visible;
        let mut shown = format!("{} ({opcode}) | Takes {} arguments.\n", v(&name), params.len());
        if !text(c, "annotation").is_empty() {
            lines += &format!("Description: {}\n", text(c, "annotation"));
            shown += &format!("Description: {}\n", v(text(c, "annotation")));
        }
        for p in params {
            let (arg, kind) = (text(p, "name"), text(&p["type"], "name"));
            let description = match text(p, "annotation") {
                "" => "--no description--",
                description => description,
            };
            lines += &format!("\t{arg} ({kind}): {description}\n");
            shown += &format!("\t{} ({}): {}\n", v(arg), v(kind), v(description));
        }
        Item { id: opcode, text: lines, shown, json: dumps(c), name }
    });
    sorted(commands.collect())
}

/// The three command names closest to `name`: those whose ratio to it is at least 0.6, best first.
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

/// How alike two texts are, from 0 to 1: twice the characters of their common blocks over their lengths.
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

/// The filters: identifiers, components, and a string searched in the
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
    use fgds_core::serde_json::json;

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
    fn a_received_event_is_a_line_or_its_record() {
        let record = json!({
            "seq": "4", "kind": "fprime.event", "id": 16777217,
            "flight_time": {"base": 2, "context": 0, "seconds": 1795275108, "microseconds": 643},
            "body": {"name": "CdhCore.cmdDisp.OpCodeCompleted", "severity": "COMMAND", "args": {"Opcode": 1},
                     "display_text": "Opcode 0x1 completed \u{2028}"},
        });
        let item = Item::from_record(&record);
        assert_eq!(
            item.line(false),
            "2026-11-21T15:31:48.000643: CdhCore.cmdDisp.OpCodeCompleted (16777217) (2(0)-1795275108:643) COMMAND : Opcode 0x1 completed \u{2028}"
        );
        // The JSON is the record, in ASCII.
        assert!(item.line(true).is_ascii() && item.line(true).contains("\\u2028"));
        assert_eq!(fgds_core::serde_json::from_str::<Value>(&item.line(true)).unwrap(), record);
    }

    #[test]
    fn a_received_channel_sample_is_a_line_or_its_record() {
        let record = json!({"seq": "5", "kind": "fprime.channel", "id": 7,
            "body": {"name": "a.b.Count", "value": "18446744073709551615", "display_text": "9 m"},
            "flight_time": {"base": 1, "context": 3, "seconds": 12, "microseconds": 5}});
        let item = Item::from_record(&record);
        assert_eq!(item.line(false), "TB_PROC_TIME: 12.000005s, context=3: a.b.Count (7) (1(3)-12:5) 9 m");
        assert_eq!(fgds_core::serde_json::from_str::<Value>(&item.line(true)).unwrap(), record);
    }

    #[test]
    fn definitions_are_sorted_by_identifier() {
        let events = events(&dictionary());
        assert_eq!(events.iter().map(|e| e.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(events[1].line(false), "a.b.Second (2) WARNING_HI : Second event");
        assert_eq!(channels(&dictionary())[0].line(false), "a.b.Count (7) : A count");
        let commands = commands(&dictionary());
        assert_eq!(commands[0].name, "a.b.STOP");
        assert_eq!(
            commands[1].line(false),
            "a.b.GO (16) | Takes 2 arguments.\nDescription: Go\n\tspeed (U8): How fast\n\tmode (a.Mode): --no description--\n"
        );
        assert_eq!(
            fgds_core::serde_json::from_str::<Value>(&commands[1].line(true)).unwrap(),
            dictionary()["commands"][0]
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
