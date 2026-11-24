//! `fgds merge-dictionary`: the function of `fprime-merge-dictionary`.
//!
//! Two deployments joined by a hub are operated with one dictionary. An identifier names an item: entries with the
//! same identifier are one item, entries sharing a name but not an identifier are both kept under
//! `<prefix>.<name>`. The output is JSON indented by two spaces.

use std::collections::{BTreeSet, HashMap, HashSet};

use fgds_core::serde_json::{Map, Value};

const ID_SECTIONS: [(&str, &str); 6] = [
    ("commands", "opcode"),
    ("parameters", "id"),
    ("events", "id"),
    ("telemetryChannels", "id"),
    ("records", "id"),
    ("containers", "id"),
];
const QUALIFIED_NAME_SECTIONS: [&str; 2] = ["typeDefinitions", "constants"];
const PACKET_SETS: &str = "telemetryPacketSets";
const VERSION_FIELDS: [&str; 3] = ["projectVersion", "frameworkVersion", "dictionarySpecVersion"];

#[derive(Default, Clone)]
pub struct Options {
    pub name: Option<String>,
    pub permissive: bool,
    pub prefer_primary: bool,
    pub no_namespace: bool,
    pub namespace_all: bool,
}

pub struct Input {
    pub path: String,
    pub data: Value,
    pub prefix: Option<String>,
}

#[derive(Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

fn id_key(section: &str) -> Option<&'static str> {
    ID_SECTIONS.iter().find(|(name, _)| *name == section).map(|(_, key)| *key)
}

fn array_sections() -> impl Iterator<Item = &'static str> {
    QUALIFIED_NAME_SECTIONS.into_iter().chain(ID_SECTIONS.iter().map(|(name, _)| *name)).chain([PACKET_SETS])
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

pub fn is_dotted_identifier(text: &str) -> bool {
    text.split('.').all(is_identifier)
}

fn is_integer(value: Option<&Value>) -> bool {
    value.is_some_and(|v| v.is_i64() || v.is_u64())
}

fn hex(value: &Value) -> String {
    match (value.as_u64(), value.as_i64()) {
        (Some(id), _) => format!("{id:#x}"),
        (_, Some(id)) => format!("-{:#x}", id.unsigned_abs()),
        _ => value.to_string(),
    }
}

fn text<'a>(entry: &'a Value, key: &str) -> &'a str {
    entry.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn entries<'a>(data: &'a Value, section: &str) -> &'a [Value] {
    data.get(section).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn list(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn with(entry: &Value, key: &str, value: Value) -> Value {
    let mut entry = entry.as_object().cloned().unwrap_or_default();
    entry.insert(key.to_string(), value);
    Value::Object(entry)
}

fn valid_packet_set(packet_set: &Value) -> bool {
    fn names(value: Option<&Value>) -> bool {
        match value {
            None | Some(Value::Null) => true,
            Some(Value::Array(names)) => names.iter().all(Value::is_string),
            Some(_) => false,
        }
    }
    let packet = |packet: &Value| {
        packet.is_object()
            && packet.get("name").is_some_and(Value::is_string)
            && is_integer(packet.get("id"))
            && names(packet.get("members"))
    };
    let packets = match packet_set.get("members") {
        None | Some(Value::Null) => true,
        Some(Value::Array(packets)) => packets.iter().all(packet),
        Some(_) => false,
    };
    packets && names(packet_set.get("omitted"))
}

/// One `Malformed dictionary` error for each structural problem of an input.
fn validate(input: &Input, report: &mut Report) {
    let mut malformed =
        |problem: String| report.errors.push(format!("Malformed dictionary '{}': {problem}", input.path));
    let Some(data) = input.data.as_object() else {
        return malformed("must be a JSON object".into());
    };
    match data.get("metadata").and_then(Value::as_object) {
        None => malformed("'metadata' must be an object".into()),
        Some(metadata) if metadata.get("deploymentName").is_some_and(|name| !name.is_string()) => {
            malformed("'metadata.deploymentName' must be a string".into())
        }
        Some(_) => {}
    }
    for section in array_sections() {
        let Some(entries) = data.get(section).and_then(Value::as_array) else {
            malformed(format!("'{section}' must be an array"));
            continue;
        };
        let name_key = if QUALIFIED_NAME_SECTIONS.contains(&section) { "qualifiedName" } else { "name" };
        let id_key = id_key(section);
        for (position, entry) in entries.iter().enumerate() {
            let name = text(entry, name_key);
            if !entry.is_object() || name.is_empty() {
                malformed(format!("'{section}' entry #{position} must have a non-empty string '{name_key}'"));
            } else if id_key.is_some_and(|key| !is_integer(entry.get(key))) {
                malformed(format!("'{section}' entry '{name}' must have an integer '{}'", id_key.unwrap_or_default()));
            } else if section == PACKET_SETS && !valid_packet_set(entry) {
                malformed(format!(
                    "'{section}' entry '{name}' must have packets with a string 'name', an integer 'id' and arrays of channel names"
                ));
            }
        }
        for key in [Some(name_key), id_key].into_iter().flatten() {
            let mut seen = HashSet::new();
            let twice: BTreeSet<String> = entries
                .iter()
                .filter(|entry| entry.is_object())
                .map(|entry| entry.get(key).unwrap_or(&Value::Null).to_string())
                .filter(|value| !seen.insert(value.clone()))
                .collect();
            for value in twice {
                malformed(format!("'{section}' has two entries with {key} {value}"));
            }
        }
    }
}

struct Merger<'a> {
    inputs: [&'a Input; 2],
    options: &'a Options,
    report: Report,
    /// Per input: section and name of the entries renamed `<prefix>.<name>`.
    renamed: [HashSet<(String, String)>; 2],
    /// Channel names of the second input that leave the merge: the packets listing them leave too.
    dropped_channels: HashSet<String>,
    /// Channel names of the second input that are the first input's entry (same identifier).
    shared_channels: HashSet<String>,
    prefixes: Option<[String; 2]>,
    /// A prefix is missing or shared: the renames mean nothing, and are not reported.
    prefix_error: bool,
}

impl Merger<'_> {
    fn merge(&mut self) -> Option<Value> {
        for input in self.inputs {
            validate(input, &mut self.report);
        }
        if !self.report.errors.is_empty() {
            return None;
        }
        let [primary, secondary] = self.inputs;
        let mut merged = Map::new();
        merged.insert("metadata".into(), self.metadata());
        for section in QUALIFIED_NAME_SECTIONS {
            merged.insert(section.into(), Value::Array(self.by_qualified_name(section)));
        }
        for (section, _) in ID_SECTIONS {
            let entries = self.entries(section, entries(&primary.data, section), entries(&secondary.data, section));
            merged.insert(section.into(), Value::Array(entries));
        }
        let sets: Vec<Vec<Value>> = (0..2)
            .map(|index| {
                entries(&self.inputs[index].data, PACKET_SETS).iter().map(|set| self.packet_set(index, set)).collect()
            })
            .collect();
        let sets = self.entries(PACKET_SETS, &sets[0], &sets[1]);
        self.check_packet_sets(&sets, list(merged.get("telemetryChannels")));
        merged.insert(PACKET_SETS.into(), Value::Array(sets));
        // Other top-level keys: in the second input's order, then the first's; the first input's value wins.
        for data in [&secondary.data, &primary.data] {
            for key in data.as_object().into_iter().flat_map(Map::keys) {
                if !merged.contains_key(key) {
                    let value = primary.data.get(key).or_else(|| secondary.data.get(key)).cloned().unwrap_or_default();
                    merged.insert(key.clone(), value);
                }
            }
        }
        self.report.errors.is_empty().then_some(Value::Object(merged))
    }

    /// The first input's metadata under the new name; the versions must agree unless `--permissive`.
    fn metadata(&mut self) -> Value {
        let [primary, secondary] = self.inputs;
        let (first, second) = (&primary.data["metadata"], &secondary.data["metadata"]);
        let name = match &self.options.name {
            Some(name) => {
                if !is_dotted_identifier(name) {
                    self.report.errors.push(format!("deploymentName '{name}' is not a valid dotted identifier"));
                }
                name.clone()
            }
            None => {
                let of = |metadata: &Value| {
                    metadata.get("deploymentName").and_then(Value::as_str).unwrap_or("unknown").to_string()
                };
                format!("{}_{}_merged", of(first), of(second))
            }
        };
        for version in VERSION_FIELDS {
            let (one, other) =
                (first.get(version).unwrap_or(&Value::Null), second.get(version).unwrap_or(&Value::Null));
            if one != other {
                let problem = format!(
                    "Inconsistent metadata values for field '{version}': {one} in {}, {other} in {}; keeping {}'s",
                    primary.path, secondary.path, primary.path
                );
                if self.options.permissive { &mut self.report.warnings } else { &mut self.report.errors }.push(problem);
            }
        }
        if first.get("libraryVersions") != second.get("libraryVersions") {
            self.report.warnings.push(format!(
                "metadata libraryVersions differ between {} and {}; keeping {}'s",
                primary.path, secondary.path, primary.path
            ));
        }
        with(first, "deploymentName", Value::String(name))
    }

    /// Type definitions and constants: one copy for a qualified name; two definitions of a name are a conflict.
    fn by_qualified_name(&mut self, section: &str) -> Vec<Value> {
        let [primary, secondary] = self.inputs;
        let mut merged = entries(&primary.data, section).to_vec();
        let held: HashMap<&str, &Value> =
            entries(&primary.data, section).iter().map(|e| (text(e, "qualifiedName"), e)).collect();
        for entry in entries(&secondary.data, section) {
            let name = text(entry, "qualifiedName");
            match held.get(name) {
                None => merged.push(entry.clone()),
                Some(held) if *held != entry => {
                    let conflict = format!(
                        "{section} '{name}' has inconsistent definitions in {} and {}",
                        primary.path, secondary.path
                    );
                    let warning = format!("{conflict}; kept definition from {}", primary.path);
                    self.drop(section, entry, conflict, warning, false);
                }
                Some(_) => {}
            }
        }
        merged
    }

    /// The entries of a section of both inputs. Where entries carry an identifier it decides: identical entries
    /// are one; the same identifier otherwise is a conflict; the same name with another identifier renames both.
    fn entries(&mut self, section: &str, primary_entries: &[Value], secondary_entries: &[Value]) -> Vec<Value> {
        let [primary, secondary] = self.inputs;
        let id_key = id_key(section);
        let by_name: HashMap<&str, &Value> = primary_entries.iter().map(|e| (text(e, "name"), e)).collect();
        let by_id: HashMap<String, &Value> =
            id_key.map(|key| primary_entries.iter().map(|e| (e[key].to_string(), e)).collect()).unwrap_or_default();
        let prefixed_anyway = id_key.is_some() && self.options.namespace_all;
        let located = |input: &Input, entry: &Value| match id_key {
            Some(key) => format!("{} ({})", input.path, hex(&entry[key])),
            None => input.path.clone(),
        };
        let mut kept = Vec::new();
        for entry in secondary_entries {
            let name = text(entry, "name");
            let held = by_name.get(name).copied();
            let same_id = id_key.and_then(|key| by_id.get(&entry[key].to_string()).copied());
            if held == Some(entry) {
                if section == "telemetryChannels" {
                    self.shared_channels.insert(name.to_string());
                }
                continue;
            }
            match (same_id, held) {
                (Some(same), _) if text(same, "name") == name => {
                    let conflict = format!(
                        "{section} '{name}' has different definitions in {} and {}",
                        primary.path, secondary.path
                    );
                    let warning = format!("{conflict}; kept definition '{name}' from {}", primary.path);
                    self.drop(section, entry, conflict, warning, true);
                }
                (Some(same), _) => {
                    let key = id_key.unwrap_or_default();
                    let other = text(same, "name");
                    let conflict = format!(
                        "{section} {key} {} is used by '{other}' in {} and '{name}' in {}",
                        hex(&entry[key]),
                        primary.path,
                        secondary.path
                    );
                    let warning = format!("{conflict}; '{name}' dropped in favour of '{other}'");
                    self.drop(section, entry, conflict, warning, false);
                }
                (None, Some(held)) if !prefixed_anyway => {
                    let conflict = format!(
                        "{section} '{name}' is in {} and {} with different {}s",
                        located(primary, held),
                        located(secondary, entry),
                        id_key.unwrap_or("definition")
                    );
                    if self.options.no_namespace {
                        let warning =
                            format!("{conflict}; {}'s dropped in favour of {}'s", secondary.path, primary.path);
                        self.drop(section, entry, format!("{conflict}; use --prefer-primary"), warning, false);
                    } else {
                        for renamed in &mut self.renamed {
                            renamed.insert((section.to_string(), name.to_string()));
                        }
                        let (alpha, beta) = (self.output_name(0, section, name), self.output_name(1, section, name));
                        if !self.prefix_error {
                            self.report.warnings.push(format!("{conflict}; renamed to '{alpha}' and '{beta}'"));
                        }
                        kept.push(entry.clone());
                    }
                }
                _ => kept.push(entry.clone()),
            }
        }
        let mut merged = Vec::new();
        for (index, side) in [primary_entries, kept.as_slice()].into_iter().enumerate() {
            for entry in side {
                let name = self.output_name(index, section, text(entry, "name"));
                merged.push(with(entry, "name", Value::String(name)));
            }
        }
        if self.prefix_error {
            return merged;
        }
        let mut seen = HashSet::new();
        let twice: BTreeSet<&str> =
            merged.iter().map(|entry| text(entry, "name")).filter(|name| !seen.insert(*name)).collect();
        for name in twice {
            self.report.errors.push(format!(
                "cannot rename {section} entries to '{name}': the merged dictionary would hold two entries with that name"
            ));
        }
        merged
    }

    /// The second input's entry loses a conflict: an error, or with `--prefer-primary` a warning and it is left out.
    /// Packets naming a channel left out go too, unless the first input keeps that name at that identifier.
    fn drop(&mut self, section: &str, entry: &Value, error: String, warning: String, name_kept: bool) {
        if !self.options.prefer_primary {
            return self.report.errors.push(error);
        }
        self.report.warnings.push(warning);
        if section == "telemetryChannels" {
            let channels = if name_kept { &mut self.shared_channels } else { &mut self.dropped_channels };
            channels.insert(text(entry, "name").to_string());
        }
    }

    /// The prefix of an input: `--prefix`, else the last part of its deployment name. A problem is reported once,
    /// the first time a prefix is needed.
    fn prefix(&mut self, index: usize) -> String {
        if self.prefixes.is_none() {
            let found = self.inputs.map(|input| {
                let name = text(&input.data["metadata"], "deploymentName");
                let segment = name.rsplit('.').next().unwrap_or_default();
                input
                    .prefix
                    .clone()
                    .filter(|p| !p.is_empty())
                    .or_else(|| is_identifier(segment).then(|| segment.to_string()))
            });
            let before = self.report.errors.len();
            for (input, prefix) in self.inputs.iter().zip(&found) {
                if prefix.is_none() {
                    self.report.errors.push(format!(
                        "cannot derive a namespace prefix for {}: metadata.deploymentName must end in an identifier; use --prefix",
                        input.path
                    ));
                }
            }
            if found[0].is_some() && found[0] == found[1] {
                self.report.errors.push(format!(
                    "{} and {} both have the namespace prefix '{}'; use --prefix to tell them apart",
                    self.inputs[0].path,
                    self.inputs[1].path,
                    found[0].as_deref().unwrap_or_default()
                ));
            }
            self.prefix_error = self.report.errors.len() > before;
            self.prefixes = Some(found.map(|prefix| prefix.unwrap_or_else(|| "None".to_string())));
        }
        self.prefixes.as_ref().map(|prefixes| prefixes[index].clone()).unwrap_or_default()
    }

    /// The name an entry of a section of an input has in the merged dictionary.
    fn output_name(&mut self, index: usize, section: &str, name: &str) -> String {
        let prefixed = self.options.namespace_all && id_key(section).is_some();
        if prefixed || self.renamed[index].contains(&(section.to_string(), name.to_string())) {
            return format!("{}.{name}", self.prefix(index));
        }
        name.to_string()
    }

    /// A packet set with the new channel names of its input, without the packets naming a channel left out.
    fn packet_set(&mut self, index: usize, packet_set: &Value) -> Value {
        let dropped = if index == 1 { self.dropped_channels.clone() } else { HashSet::new() };
        let channel = |merger: &mut Self, name: &Value| {
            let name = name.as_str().unwrap_or_default();
            let from = if merger.shared_channels.contains(name) { 0 } else { index };
            Value::String(merger.output_name(from, "telemetryChannels", name))
        };
        let mut packets = Vec::new();
        for packet in list(packet_set.get("members")) {
            let members = list(packet.get("members"));
            let names: BTreeSet<&str> = members.iter().filter_map(Value::as_str).collect();
            let lost: Vec<String> =
                names.iter().filter(|name| dropped.contains(**name)).map(|name| format!("'{name}'")).collect();
            if lost.is_empty() {
                let members = members.iter().map(|name| channel(self, name)).collect();
                packets.push(with(packet, "members", Value::Array(members)));
            } else {
                let survivors = names.len() - lost.len();
                let lost_too = if survivors > 0 {
                    format!(", so its other {survivors} channel(s) are lost too")
                } else {
                    String::new()
                };
                self.report.warnings.push(format!(
                    "packet '{}' of packet set '{}' in {} removed because it references dropped channel(s) {}; the GDS will discard that packet{lost_too}",
                    text(packet, "name"),
                    text(packet_set, "name"),
                    self.inputs[index].path,
                    lost.join(", ")
                ));
            }
        }
        let omitted = list(packet_set.get("omitted"))
            .iter()
            .filter(|name| !name.as_str().is_some_and(|name| dropped.contains(name)))
            .map(|name| channel(self, name))
            .collect();
        with(&with(packet_set, "members", Value::Array(packets)), "omitted", Value::Array(omitted))
    }

    /// Every channel a packet set names is in the merged channels.
    fn check_packet_sets(&mut self, packet_sets: &[Value], channels: &[Value]) {
        if !self.report.errors.is_empty() {
            return;
        }
        if packet_sets.len() > 1 {
            self.report.warnings.push(format!(
                "merged dictionary has {} packet sets; the GDS needs a packet set name to start and decodes only the selected set",
                packet_sets.len()
            ));
        }
        let channels: HashSet<&str> = channels.iter().map(|channel| text(channel, "name")).collect();
        for packet_set in packet_sets {
            let named: BTreeSet<&str> = list(packet_set.get("members"))
                .iter()
                .flat_map(|packet| list(packet.get("members")))
                .chain(list(packet_set.get("omitted")))
                .filter_map(Value::as_str)
                .collect();
            for name in named.into_iter().filter(|name| !channels.contains(name)) {
                self.report
                    .errors
                    .push(format!("packet set '{}' references unknown channel '{name}'", text(packet_set, "name")));
            }
        }
    }
}

/// The merged dictionary, or none when the report has an error.
pub fn merge(primary: &Input, secondary: &Input, options: &Options) -> (Option<Value>, Report) {
    let mut merger = Merger {
        inputs: [primary, secondary],
        options,
        report: Report::default(),
        renamed: Default::default(),
        dropped_channels: HashSet::new(),
        shared_channels: HashSet::new(),
        prefixes: None,
        prefix_error: false,
    };
    let merged = merger.merge();
    (merged, merger.report)
}
