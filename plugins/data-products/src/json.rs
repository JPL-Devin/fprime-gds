//! The decoded document for the network API (API-4).

use fgds_core::serde_json::Value;

/// The document with the integers a browser cannot hold exactly as text.
pub fn for_api(value: &Value) -> Value {
    const SAFE: u64 = (1 << 53) - 1;
    match value {
        Value::Number(n)
            if n.as_u64().is_some_and(|u| u > SAFE) || n.as_i64().is_some_and(|i| i.unsigned_abs() > SAFE) =>
        {
            Value::String(n.to_string())
        }
        Value::Array(items) => items.iter().map(for_api).collect(),
        Value::Object(map) => map.iter().map(|(key, value)| (key.clone(), for_api(value))).collect(),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgds_core::serde_json::json;

    #[test]
    fn large_integers_are_text_for_the_api() {
        let document =
            json!({"a": [9007199254740991u64, 9007199254740992u64, -9007199254740992i64], "b": 1.5, "c": u64::MAX});
        assert_eq!(
            for_api(&document),
            json!({"a": [9007199254740991u64, "9007199254740992", "-9007199254740992"], "b": 1.5, "c": "18446744073709551615"})
        );
    }
}
