use crate::{Error, Result};
use base64::Engine;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Name of a record kind, `<namespace>.<name>`.
pub type KindId = String;

// Kinds the core defines.
pub const KIND_DISCARDED: &str = "raw.discarded";
pub const KIND_DECODE_ERROR: &str = "gds.decode_error";
pub const KIND_UNKNOWN_PACKET: &str = "gds.unknown_packet";
pub const KIND_PLUGIN_FAULT: &str = "gds.plugin_fault";

/// Ground time: microseconds since the Unix epoch, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Timestamp(pub i64);

impl Timestamp {
    pub fn now() -> Self {
        let now = time::OffsetDateTime::now_utc();
        Timestamp((now.unix_timestamp_nanos() / 1000) as i64)
    }

    /// ISO 8601 UTC with microseconds (API-4).
    pub fn to_iso(self) -> String {
        const FORMAT: &str = "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:6]Z";
        time::OffsetDateTime::from_unix_timestamp_nanos(self.0 as i128 * 1000)
            .ok()
            .zip(time::format_description::parse_borrowed::<2>(FORMAT).ok())
            .and_then(|(t, format)| t.format(&format).ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlightTime {
    pub base: u16,
    pub context: u8,
    pub seconds: u32,
    pub microseconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetaValue {
    U64(u64),
    Text(String),
}

/// Optional facts about a packet, set by framing layers (LNK-19). The core knows no key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PacketMeta(pub BTreeMap<&'static str, MetaValue>);

impl PacketMeta {
    pub fn with(mut self, key: &'static str, value: MetaValue) -> Self {
        self.0.insert(key, value);
        self
    }

    pub fn get(&self, key: &str) -> Option<&MetaValue> {
        self.0.get(key)
    }

    /// Add the entries of `inner`, the layer after this one.
    pub fn extend(&mut self, inner: PacketMeta) {
        self.0.extend(inner.0);
    }

    fn to_json(&self) -> Value {
        let entries = self.0.iter().map(|(key, value)| {
            let value = match value {
                MetaValue::U64(n) => json!(n.to_string()),
                MetaValue::Text(s) => json!(s),
            };
            (key.to_string(), value)
        });
        Value::Object(entries.collect())
    }
}

/// The one data model shared by plugins, storage, the API and clients.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub seq: u64,
    pub run: u32,
    pub ground_time: Timestamp,
    pub kind: KindId,
    pub flight_time: Option<FlightTime>,
    pub id: Option<u32>,
    pub entity: Option<String>,
    pub stored: bool,
    pub raw: Bytes,
    pub meta: PacketMeta,
    pub body: Value,
}

impl Record {
    /// Typed access to the body for the plugin that owns the kind.
    pub fn body_as<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.body.clone()).map_err(Error::other)
    }

    /// The record as the network API carries it (API-4).
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq.to_string(),
            "run": self.run,
            "ground_time": self.ground_time.to_iso(),
            "kind": self.kind,
            "flight_time": self.flight_time.map(|t| json!({
                "base": t.base, "context": t.context,
                "seconds": t.seconds, "microseconds": t.microseconds,
            })),
            "id": self.id,
            "entity": self.entity,
            "stored": self.stored,
            "raw": base64::engine::general_purpose::STANDARD.encode(&self.raw),
            "meta": self.meta.to_json(),
            "body": self.body,
        })
    }
}

/// A record before the pipeline numbers and stamps it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewRecord {
    pub kind: KindId,
    pub flight_time: Option<FlightTime>,
    pub id: Option<u32>,
    pub entity: Option<String>,
    pub raw: Bytes,
    pub meta: PacketMeta,
    pub body: Value,
}

impl NewRecord {
    pub fn new(kind: &str, body: Value) -> Self {
        NewRecord { kind: kind.to_string(), body, ..Default::default() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Number,
    Text,
    Bool,
}

/// A body field a client may filter on.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub name: &'static str,
    /// JSON pointer into the body, e.g. `/severity`.
    pub pointer: &'static str,
    pub ty: FieldType,
}

#[derive(Debug, Clone)]
pub struct KindSpec {
    pub id: KindId,
    /// JSON Schema of the body.
    pub schema: &'static str,
    /// Delivered to live subscribers only; never stored or replayed.
    pub transient: bool,
    pub description: &'static str,
    pub fields: &'static [FieldSpec],
    /// JSON pointers of the body fields searched by a free-text filter.
    pub text_fields: &'static [&'static str],
    /// Records update one entity; current state is the latest record per entity.
    pub has_entities: bool,
}

impl KindSpec {
    /// A stored kind with no declared fields.
    pub fn new(id: &str, description: &'static str) -> Self {
        KindSpec {
            id: id.to_string(),
            schema: "{}",
            transient: false,
            description,
            fields: &[],
            text_fields: &[],
            has_entities: false,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "description": self.description,
            "schema": serde_json::from_str::<Value>(self.schema).unwrap_or(Value::Null),
            "transient": self.transient,
            "has_entities": self.has_entities,
            "fields": self.fields.iter().map(|f| json!({
                "name": f.name, "pointer": f.pointer,
                "type": format!("{:?}", f.ty).to_lowercase(),
            })).collect::<Vec<_>>(),
            "text_fields": self.text_fields,
        })
    }
}

/// The kinds the core itself publishes.
pub(crate) fn core_kinds() -> Vec<KindSpec> {
    let text =
        |id: &str, description| KindSpec { text_fields: &["/reason", "/message"], ..KindSpec::new(id, description) };
    vec![
        text(KIND_DISCARDED, "Link bytes a framing layer could not use"),
        text(KIND_DECODE_ERROR, "A packet its decoder could not decode"),
        text(KIND_UNKNOWN_PACKET, "A packet no decoder matches"),
        text(KIND_PLUGIN_FAULT, "A panic caught in plugin code"),
    ]
}

/// Which records a subscriber receives. No kinds means every kind.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub kinds: Vec<KindId>,
}

impl Filter {
    pub fn kinds(kinds: &[&str]) -> Self {
        Filter { kinds: kinds.iter().map(|k| k.to_string()).collect() }
    }

    pub fn matches(&self, record: &Record) -> bool {
        self.kinds.is_empty() || self.kinds.contains(&record.kind)
    }
}

/// Ground-time range: `from` inclusive, `to` exclusive.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimeRange {
    pub from: Option<Timestamp>,
    pub to: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
}

impl std::str::FromStr for Op {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "eq" => Op::Eq,
            "ne" => Op::Ne,
            "lt" => Op::Lt,
            "le" => Op::Le,
            "gt" => Op::Gt,
            "ge" => Op::Ge,
            "contains" => Op::Contains,
            _ => return Err(Error::Invalid(format!("unknown filter operator \"{s}\""))),
        })
    }
}

/// A condition on a field declared in the record's `KindSpec`.
#[derive(Debug, Clone)]
pub struct FieldFilter {
    pub field: String,
    pub op: Op,
    pub value: Value,
}

/// A structured history query. Every field given must match.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Empty means every kind.
    pub kinds: Vec<KindId>,
    /// `None` means the current run.
    pub run: Option<u32>,
    /// Exclusive lower bound.
    pub after_seq: Option<u64>,
    /// Exclusive upper bound.
    pub before_seq: Option<u64>,
    /// Empty means every identifier.
    pub ids: Vec<u32>,
    pub time: Option<TimeRange>,
    pub filters: Vec<FieldFilter>,
    /// Case-insensitive substring of the kind's text fields.
    pub text: Option<String>,
    /// Largest number of records in the page; at least 1.
    pub limit: u32,
}

/// One page of a query, in ascending `seq` order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    pub records: Vec<Record>,
    /// Set when more records match: pass it as `after_seq` for the next page.
    pub next: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunInfo {
    pub run: u32,
    pub started: Timestamp,
    pub gds_version: String,
    pub dictionary_version: String,
    pub plugins: Vec<String>,
    /// First sequence number the pipeline may assign in this run (STO-14).
    pub first_seq: u64,
}

impl RunInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "run": self.run,
            "started": self.started.to_iso(),
            "gds_version": self.gds_version,
            "dictionary_version": self.dictionary_version,
            "plugins": self.plugins,
        })
    }
}

/// A number given as a JSON number or as numeric text (64-bit values are text, API-4).
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl FieldFilter {
    /// Compare a field value with the filter value: as numbers when both are numeric, else as text.
    fn accepts(&self, field: &Value) -> bool {
        use std::cmp::Ordering::*;
        if self.op == Op::Contains {
            return as_text(field).contains(&as_text(&self.value));
        }
        let order = match (as_number(field), as_number(&self.value)) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => Some(as_text(field).cmp(&as_text(&self.value))),
        };
        match self.op {
            Op::Eq => order == Some(Equal),
            Op::Ne => order != Some(Equal),
            Op::Lt => order == Some(Less),
            Op::Le => matches!(order, Some(Less | Equal)),
            Op::Gt => order == Some(Greater),
            Op::Ge => matches!(order, Some(Greater | Equal)),
            Op::Contains => unreachable!(),
        }
    }
}

impl Query {
    /// Reject a query that filters on a field no queried kind declares.
    pub fn validate(&self, kinds: &[KindSpec]) -> Result<()> {
        if self.limit == 0 {
            return Err(Error::Invalid("limit must be at least 1".into()));
        }
        if let Some(unknown) = self.kinds.iter().find(|k| !kinds.iter().any(|spec| spec.id == **k)) {
            return Err(Error::Invalid(format!("record kind \"{unknown}\" is not registered")));
        }
        for filter in &self.filters {
            let declared = kinds
                .iter()
                .filter(|k| self.kinds.is_empty() || self.kinds.contains(&k.id))
                .any(|k| k.fields.iter().any(|f| f.name == filter.field));
            if !declared {
                let name = &filter.field;
                return Err(Error::Invalid(format!("no queried kind declares field \"{name}\"")));
            }
        }
        Ok(())
    }

    /// The meaning of every query field, shared by storage plugins and fixed by the
    /// conformance suite (MNT-10). `spec` is the kind of `record`.
    pub fn matches(&self, record: &Record, spec: Option<&KindSpec>, current_run: u32) -> bool {
        let field_ok = |filter: &FieldFilter| {
            spec.and_then(|s| s.fields.iter().find(|f| f.name == filter.field))
                .and_then(|f| record.body.pointer(f.pointer))
                .is_some_and(|value| filter.accepts(value))
        };
        let text_ok = |text: &String| {
            let needle = text.to_lowercase();
            spec.is_some_and(|s| {
                s.text_fields.iter().any(|pointer| {
                    record.body.pointer(pointer).is_some_and(|v| as_text(v).to_lowercase().contains(&needle))
                })
            })
        };
        (self.kinds.is_empty() || self.kinds.contains(&record.kind))
            && record.run == self.run.unwrap_or(current_run)
            && self.after_seq.is_none_or(|s| record.seq > s)
            && self.before_seq.is_none_or(|s| record.seq < s)
            && (self.ids.is_empty() || record.id.is_some_and(|id| self.ids.contains(&id)))
            && self.time.is_none_or(|t| {
                t.from.is_none_or(|from| record.ground_time >= from) && t.to.is_none_or(|to| record.ground_time < to)
            })
            && self.filters.iter().all(field_ok)
            && self.text.as_ref().is_none_or(text_ok)
    }
}
