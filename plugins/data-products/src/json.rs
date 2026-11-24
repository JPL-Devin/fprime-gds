//! The JSON document `fgds dp decode` writes, as a value that keeps integers apart from floats.

use fgds_core::serde_json::{self, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    List(Vec<Json>),
    /// Keys in insertion order, as a Python dict.
    Map(Vec<(String, Json)>),
}

impl Json {
    pub fn str(text: impl Into<String>) -> Json {
        Json::Str(text.into())
    }

    /// Set a key of a map as `dict[key] = value` does: an existing key keeps its place.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Map(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The document as JSON indented by two spaces: integers exact, a float that is no number as text.
    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(&self.to_value(false)).unwrap_or_default()
    }

    /// The same document for the network API (API-4): integers a browser cannot hold
    /// exactly become strings too.
    pub fn to_api(&self) -> Value {
        self.to_value(true)
    }

    fn to_value(&self, api: bool) -> Value {
        const SAFE: i128 = (1 << 53) - 1;
        match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Int(i) if api && !(-SAFE..=SAFE).contains(i) => Value::String(i.to_string()),
            Json::Int(i) => match (i64::try_from(*i), u64::try_from(*i)) {
                (Ok(i), _) => Value::from(i),
                (_, Ok(u)) => Value::from(u),
                _ => Value::String(i.to_string()),
            },
            Json::Float(f) => fgds_core::dictionary::float_json(*f),
            Json::Str(s) => Value::String(s.clone()),
            Json::List(items) => Value::Array(items.iter().map(|v| v.to_value(api)).collect()),
            Json::Map(entries) => Value::Object(entries.iter().map(|(k, v)| (k.clone(), v.to_value(api))).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_exact_in_the_file_and_text_for_a_browser() {
        let value = Json::Map(vec![
            ("d".into(), Json::Int(u64::MAX as i128)),
            ("n".into(), Json::Float(f64::NAN)),
            ("f".into(), Json::Float(1.5)),
        ]);
        assert_eq!(value.to_text(), "{\n  \"d\": 18446744073709551615,\n  \"n\": \"NaN\",\n  \"f\": 1.5\n}");
        assert_eq!(value.to_api()["d"], "18446744073709551615");
    }
}
