//! Tab completion from the dictionary of the GDS.

use crate::line::WORDS;
use fgds_core::serde_json::Value;

/// The names of a dictionary, sorted.
#[derive(Default)]
pub struct Names {
    /// Each command with, for each of its parameters, the values that can be completed.
    pub commands: Vec<(String, Vec<Vec<String>>)>,
    pub events: Vec<String>,
    pub channels: Vec<String>,
}

fn list<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}

/// The constants of an enumeration type (through aliases), `true` and `false` of a boolean.
fn choices<'a>(dictionary: &'a Value, mut ty: &'a Value) -> Vec<String> {
    for _ in 0..8 {
        if ty["kind"] == "bool" {
            return vec!["true".into(), "false".into()];
        }
        let Some(definition) = list(dictionary, "typeDefinitions").find(|t| t["qualifiedName"] == ty["name"]) else {
            break;
        };
        match definition["kind"].as_str() {
            Some("alias") => ty = &definition["underlyingType"],
            Some("enum") => {
                return list(definition, "enumeratedConstants")
                    .filter_map(|c| c["name"].as_str())
                    .map(str::to_string)
                    .collect()
            }
            _ => break,
        }
    }
    Vec::new()
}

impl Names {
    pub fn of(dictionary: &Value) -> Names {
        let names = |key: &str| {
            let mut names: Vec<String> =
                list(dictionary, key).filter_map(|d| d["name"].as_str()).map(str::to_string).collect();
            names.sort();
            names
        };
        let mut commands: Vec<(String, Vec<Vec<String>>)> = list(dictionary, "commands")
            .filter_map(|c| {
                let parameters = list(c, "formalParams").map(|p| choices(dictionary, &p["type"])).collect();
                Some((c["name"].as_str()?.to_string(), parameters))
            })
            .collect();
        commands.sort();
        Names { commands, events: names("events"), channels: names("telemetryChannels") }
    }

    /// What Tab offers for `line` with the cursor at byte `pos`: where the word under the cursor
    /// starts, and the texts that can replace it. A name is offered when it starts with the word,
    /// or when its last part does (`cmd CMD_NO` offers `CdhCore.cmdDisp.CMD_NO_OP`).
    pub fn complete(&self, line: &str, pos: usize) -> (usize, Vec<String>) {
        let head = &line[..pos];
        let start = head.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        let word = &head[start..];
        let before: Vec<&str> = head[..start].split_whitespace().collect();
        let fixed = |words: &[&str]| words.iter().filter(|w| w.starts_with(word)).map(|w| w.to_string()).collect();
        let named = |names: &mut dyn Iterator<Item = &String>| {
            let fits = |name: &&String| {
                name.starts_with(word) || name.rsplit('.').next().is_some_and(|last| last.starts_with(word))
            };
            names.filter(fits).cloned().collect::<Vec<_>>()
        };
        let commands = || named(&mut self.commands.iter().map(|(name, _)| name));
        let all = || named(&mut self.commands.iter().map(|(name, _)| name).chain(&self.events).chain(&self.channels));
        let found = match before.as_slice() {
            [] => fixed(WORDS),
            ["cmd"] | ["cmds"] | ["dict", "commands" | "cmds"] => commands(),
            ["cmd", name, given @ ..] => self
                .commands
                .iter()
                .find(|(command, _)| command == name)
                .and_then(|(_, parameters)| parameters.get(given.len()))
                .map_or(Vec::new(), |values| values.iter().filter(|v| v.starts_with(word)).cloned().collect()),
            ["help"] => all(),
            ["events"] | ["dict", "events"] => named(&mut self.events.iter()),
            ["channels"] | ["dict", "channels"] => named(&mut self.channels.iter()),
            ["dict"] => fixed(&["commands", "events", "channels"]),
            ["mute" | "unmute"] => {
                let mut found: Vec<String> = fixed(&["events", "channels"]);
                found.extend(named(&mut self.events.iter().chain(&self.channels)));
                found
            }
            ["subscribe" | "unsubscribe"] => named(&mut self.events.iter().chain(&self.channels)),
            _ => Vec::new(),
        };
        (start, found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgds_core::serde_json::json;

    fn names() -> Names {
        Names::of(&json!({
            "typeDefinitions": [
                {"kind": "enum", "qualifiedName": "Ref.Choice", "enumeratedConstants": [{"name": "ONE", "value": 0}, {"name": "TWO", "value": 1}]},
                {"kind": "alias", "qualifiedName": "Ref.Other", "underlyingType": {"name": "Ref.Choice", "kind": "qualifiedIdentifier"}},
            ],
            "commands": [
                {"name": "Ref.sig.SET", "formalParams": [
                    {"name": "count", "type": {"name": "U32", "kind": "integer"}},
                    {"name": "choice", "type": {"name": "Ref.Other", "kind": "qualifiedIdentifier"}},
                    {"name": "on", "type": {"name": "bool", "kind": "bool"}},
                ]},
                {"name": "Cdh.disp.CMD_NO_OP", "formalParams": []},
                {"name": "Cdh.disp.CMD_NO_OP_STRING", "formalParams": [{"name": "arg1", "type": {"name": "string", "kind": "string"}}]},
            ],
            "events": [{"name": "Cdh.disp.NoOpReceived"}, {"name": "Ref.sig.Changed"}],
            "telemetryChannels": [{"name": "Ref.sig.Count"}, {"name": "Cdh.disp.Dispatched"}],
        }))
    }

    fn complete(line: &str) -> (usize, Vec<String>) {
        names().complete(line, line.len())
    }

    #[test]
    fn the_first_word() {
        assert_eq!(complete("").1.len(), WORDS.len());
        assert_eq!(complete("c"), (0, vec!["cmd".into(), "cmds".into(), "channels".into(), "clear-filters".into()]));
        assert_eq!(complete("  mu"), (2, vec!["mute".into()]));
    }

    #[test]
    fn command_names_by_their_start_or_their_last_part() {
        assert_eq!(complete("cmd Cdh."), (4, vec!["Cdh.disp.CMD_NO_OP".into(), "Cdh.disp.CMD_NO_OP_STRING".into()]));
        assert_eq!(complete("cmd CMD_NO_OP_"), (4, vec!["Cdh.disp.CMD_NO_OP_STRING".into()]));
        assert_eq!(complete("cmd ").1.len(), 3);
        assert_eq!(complete("cmd zz").1.len(), 0);
        // The cursor inside the line: only what is before it counts.
        assert_eq!(names().complete("cmd Ref 1 2", 7), (4, vec!["Ref.sig.SET".into()]));
    }

    #[test]
    fn argument_values_of_enumerations_and_booleans() {
        assert_eq!(complete("cmd Ref.sig.SET ").1.len(), 0);
        assert_eq!(complete("cmd Ref.sig.SET 1 "), (18, vec!["ONE".into(), "TWO".into()]));
        assert_eq!(complete("cmd Ref.sig.SET 1 T"), (18, vec!["TWO".into()]));
        assert_eq!(complete("cmd Ref.sig.SET 1 TWO f"), (22, vec!["false".into()]));
        assert_eq!(complete("cmd Ref.sig.SET 1 TWO true ").1.len(), 0);
        assert_eq!(complete("cmd Unknown.NAME ").1.len(), 0);
    }

    #[test]
    fn names_of_the_lists_and_the_filters() {
        assert_eq!(complete("events No"), (7, vec!["Cdh.disp.NoOpReceived".into()]));
        assert_eq!(complete("dict ch"), (5, vec!["channels".into()]));
        assert_eq!(complete("dict channels Ref"), (14, vec!["Ref.sig.Count".into()]));
        assert_eq!(complete("mute ch"), (5, vec!["channels".into()]));
        assert_eq!(complete("mute Ref"), (5, vec!["Ref.sig.Changed".into(), "Ref.sig.Count".into()]));
        assert_eq!(complete("subscribe Cdh").1.len(), 2);
        assert_eq!(complete("help Ref").1.len(), 3);
        assert_eq!(complete("filters x").1.len(), 0);
    }
}
