//! PRM-1: `fixtures/vectors/params.json`, the results of `fprime-prm-write` and
//! `fprime-prm-decode`; and no panic on damaged files (MNT-6).

use fgds_core::serde_json::{self, Value};
use params::{to_csv, to_json, to_seq, to_text, ParamDictionary};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The result of one vector, in the shape of its expectation.
fn run(vector: &Value) -> Result<Value, String> {
    let dict = ParamDictionary::load(&fixtures().join(vector["dictionary"].as_str().unwrap()))?;
    let input = vector["input_file"].as_str().map(|file| std::fs::read(fixtures().join(file)).unwrap());
    match vector["operation"].as_str().unwrap() {
        "decode_dat" => {
            let bytes = hex(vector["input"].as_str().unwrap());
            if let Some(file) = input {
                assert_eq!(file, bytes);
            }
            let values = dict.from_dat(&bytes)?;
            let json: Value = serde_json::from_str(&to_json(&values).to_json_pretty()).unwrap();
            Ok(serde_json::json!({"json": json, "text": to_text(&values), "csv": to_csv(&values)}))
        }
        operation => {
            let file = serde_json::from_slice(&input.unwrap()).unwrap();
            let values = dict.select(&file, false)?;
            match operation {
                "encode_dat" => Ok(to_hex(&dict.to_dat(&values)?).into()),
                "encode_seq" => Ok(to_seq(&values, false).into()),
                other => panic!("unknown operation {other}"),
            }
        }
    }
}

#[test]
fn vectors() {
    let file: Value = serde_json::from_slice(&std::fs::read(fixtures().join("vectors/params.json")).unwrap()).unwrap();
    let vectors = file["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 10);
    for vector in vectors {
        let source = &vector["source"];
        match (run(vector), vector["expected_error"]["message"].as_str()) {
            (Err(message), Some(expected)) => assert_eq!(message, expected, "{source}"),
            (Ok(result), None) => {
                let expected = vector.get("expected").or(vector.get("expected_text")).unwrap();
                assert_eq!(&result, expected, "{source}");
                if let Some(file) = vector["expected_file"].as_str() {
                    assert_eq!(result, to_hex(&std::fs::read(fixtures().join(file)).unwrap()), "{source}");
                }
            }
            (result, expected) => panic!("{source}: got {result:?}, expected error {expected:?}"),
        }
    }
}

/// A dictionary with a parameter of each kind of type, and a file with a value of each.
fn every_type() -> (ParamDictionary, Value) {
    let dict = serde_json::json!({
        "typeDefinitions": [
            {"kind": "enum", "qualifiedName": "M.Mode", "representationType": {"name": "U8", "kind": "integer", "size": 8, "signed": false},
             "enumeratedConstants": [{"name": "OFF", "value": 0}, {"name": "ON", "value": 3}]},
            {"kind": "array", "qualifiedName": "M.Three", "size": 3, "elementType": {"name": "F32", "kind": "float", "size": 32}},
            {"kind": "array", "qualifiedName": "M.Modes", "size": 2, "elementType": {"name": "M.Mode", "kind": "qualifiedIdentifier"}},
            {"kind": "struct", "qualifiedName": "M.Point", "members": {
                "y": {"type": {"name": "I16", "kind": "integer", "size": 16, "signed": true}, "index": 1},
                "x": {"type": {"name": "U64", "kind": "integer", "size": 64, "signed": false}, "index": 0},
                "label": {"type": {"name": "string", "kind": "string", "size": 8}, "index": 2},
                "modes": {"type": {"name": "M.Modes", "kind": "qualifiedIdentifier"}, "index": 3},
                "raw": {"type": {"name": "U8", "kind": "integer", "size": 8, "signed": false}, "index": 4, "size": 2}}}
        ],
        "parameters": [
            {"name": "a.b.I", "id": 1, "type": {"name": "I32", "kind": "integer", "size": 32, "signed": true}, "default": -7},
            {"name": "a.b.F", "id": 2, "type": {"name": "F64", "kind": "float", "size": 64}},
            {"name": "a.b.B", "id": 3, "type": {"name": "bool", "kind": "bool"}},
            {"name": "c.E", "id": 4, "type": {"name": "M.Mode", "kind": "qualifiedIdentifier"}},
            {"name": "c.A", "id": 5, "type": {"name": "M.Three", "kind": "qualifiedIdentifier"}},
            {"name": "c.S", "id": 6, "type": {"name": "M.Point", "kind": "qualifiedIdentifier"}},
            {"name": "c.T", "id": 7, "type": {"name": "string", "kind": "string", "size": 16}}
        ]
    });
    let values = serde_json::json!({
        "a.b": {"F": 2.5, "B": true},
        "c": {"E": "ON", "A": [1, 0.5, -2], "T": "a \"b\", c",
              "S": {"x": 18446744073709551615u64, "y": -2, "label": "é", "modes": ["OFF", "ON"], "raw": [1, 255]}}
    });
    (ParamDictionary::from_json("test", dict).unwrap(), values)
}

#[test]
fn every_type_round_trips() {
    let (dict, file) = every_type();
    let values = dict.select(&file, true).unwrap();
    assert_eq!(values.len(), 7, "the default of a.b.I is included");
    let dat = dict.to_dat(&values).unwrap();
    let decoded = dict.from_dat(&dat).unwrap();
    let json: Value = serde_json::from_str(&to_json(&decoded).to_json_pretty()).unwrap();
    let mut expected = file.clone();
    expected["a.b"] = serde_json::json!({"I": -7, "F": 2.5, "B": true});
    expected["c"]["A"] = serde_json::json!([1.0, 0.5, -2.0]);
    assert_eq!(json, expected);
    // Encoding the decoded JSON gives the same file.
    assert_eq!(dict.to_dat(&dict.select(&json, false).unwrap()).unwrap(), dat);
    assert!(to_text(&decoded).contains(
        "\n\nComponent: c\n  A = [1.0, 0.5, -2.0] (type: M.Three, id: 5)\n  E = \"ON\" (type: M.Mode, id: 4)"
    ));
    assert!(to_csv(&decoded).contains("c,T,\"a \"\"b\"\", c\",String_16,7"));
    assert_eq!(
        to_seq(&values[..2], true),
        "; Autocoded sequence file from JSON\nR00:00:00 a.b.B_PRM_SET true\na.b.B_PRM_SAVE\nR00:00:00 a.b.F_PRM_SET 2.5\na.b.F_PRM_SAVE"
    );
    assert!(to_csv(&decoded).contains("c,S,\"{\"\"x\"\": 18446744073709551615, "), "{}", to_csv(&decoded));
}

#[test]
fn rejected_values_name_the_parameter() {
    let (dict, _) = every_type();
    for (file, message) in [
        (serde_json::json!({"a.b": {"I": 2147483648u32}}), "a.b.I [I32Type]: Value 2147483648 out of range!"),
        (serde_json::json!({"a.b": {"B": 1}}), "a.b.B [BoolType]: Param value is not a valid boolean"),
        (serde_json::json!({"c": {"E": "ONN"}}), "c.E [M.Mode]: Invalid enum member ONN set in ABCMeta enum!"),
        (serde_json::json!({"c": {"A": [1, 2]}}), "c.A [M.Three]: Array type <class 'fprime_gds.common.models.serialize.numerical_types.F32Type'> is of length 3, actual length 2 found!"),
        (serde_json::json!({"c": {"T": "12345678901234567"}}), "c.T [String_16]: String size 17 is greater than 16!"),
        (serde_json::json!({"c": {"X": 1}}), "Unable to find param c.X in dictionary"),
    ] {
        let result = dict.select(&file, false).and_then(|values| dict.to_dat(&values));
        assert_eq!(result.unwrap_err(), message);
    }
}

/// Every prefix of a valid file, and every single-byte change with the checksum made right
/// again, decodes or fails with a message; nothing panics.
#[test]
fn damaged_files_never_panic() {
    let (dict, file) = every_type();
    let dat = dict.to_dat(&dict.select(&file, true).unwrap()).unwrap();
    let with_checksum = |data: &[u8]| {
        let mut out = (!fgds_offline::crc32(data)).to_be_bytes().to_vec();
        out.extend_from_slice(data);
        out
    };
    let mut errors = 0;
    for end in 0..dat.len() {
        errors += usize::from(dict.from_dat(&dat[..end]).is_err());
        if end >= 4 {
            errors += usize::from(dict.from_dat(&with_checksum(&dat[4..end])).is_err());
        }
    }
    for index in 4..dat.len() {
        for change in [0x01, 0x80, 0xFF] {
            let mut data = dat[4..].to_vec();
            data[index - 4] ^= change;
            if let Err(message) = dict.from_dat(&with_checksum(&data)) {
                assert!(!message.is_empty());
                errors += 1;
            }
        }
    }
    assert!(errors > dat.len(), "{errors} errors");
    assert_eq!(
        dict.from_dat(&with_checksum(&[0xA5, 0, 0, 0, 3, 0, 0, 0, 1])).unwrap_err(),
        "Record of a.b.I at offset 5: size 3 is less than the identifier"
    );
}
