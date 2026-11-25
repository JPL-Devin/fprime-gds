//! The decoder and validator against the ground truth of `fixtures/dp`: for every
//! product and every mode recorded there, the library gives what `fprime-dp` gave.
//! The command line itself is checked by `run.sh`.

use data_products::decode::{decode, DecodeOptions, DpError};
use data_products::validate::{validate, HeaderSize, Invalid};
use data_products::DpDictionary;
use fgds_core::serde_json::{self, Value};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dp")
}

/// Every `expected.json` with the bytes and the dictionary of its product.
fn products() -> Vec<(PathBuf, Value, Vec<u8>, DpDictionary)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(fixtures().join("expected")).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let expected: Value = serde_json::from_slice(&std::fs::read(dir.join("expected.json")).unwrap()).unwrap();
        let data = fixtures().join("test_dp_data");
        let bytes = std::fs::read(data.join(expected["product"].as_str().unwrap())).unwrap();
        let dictionary = data.join(expected["dictionary"].as_str().unwrap());
        let dictionary = DpDictionary::load(dictionary.to_str().unwrap()).unwrap();
        found.push((dir, expected, bytes, dictionary));
    }
    assert_eq!(found.len(), 18);
    found
}

#[test]
fn decode_gives_the_document_or_the_error_of_fprime_dp() {
    let mut documents = 0;
    for (dir, expected, bytes, dictionary) in products() {
        for run in expected["runs"].as_array().unwrap() {
            let mode = run["mode"].as_str().unwrap();
            if !mode.starts_with("decode") {
                continue;
            }
            let options = DecodeOptions { decompress: mode == "decode", ..Default::default() };
            let context = format!("{} {mode}", dir.display());
            match (decode(&dictionary, &bytes, options), run["output_json"].as_str()) {
                (Ok(decoded), Some(file)) => {
                    let python: Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
                    let mut ours = decoded.to_json();
                    for record in ours["Records"].as_array_mut().unwrap() {
                        record["Record"].as_object_mut().unwrap().remove("record_type_name");
                    }
                    assert_eq!(ours, document(&python), "{context}: values");
                    documents += 1;
                }
                (Err(DpError::Product(message)), None) => {
                    // Both refuse; a wrong data checksum is reported before the records are read.
                    assert!(run["stderr"].as_str().unwrap().starts_with("Error: "), "{context}");
                    assert!(!message.is_empty(), "{context}");
                    assert_eq!(run["exit_code"], 1, "{context}");
                }
                (other, _) => panic!("{context}: {other:?}"),
            }
        }
    }
    assert_eq!(documents, 32);
}

/// The values of a document of `fprime-dp`, without what it says of their types: an array is
/// `{"values": [...], "value_type": ...}` there, a scalar `{"value": ..., "type": ...}`, and each member of a
/// struct carries its format and description.
fn values(python: &Value) -> Value {
    let map = python.as_object().unwrap();
    match (map.get("values"), map.get("value"), map.get("type")) {
        (Some(values), _, _) if map.contains_key("value_type") => values.clone(),
        (_, Some(value), Some(_)) => value.clone(),
        (_, _, Some(Value::String(time))) if time == "Time" => {
            map.iter().filter(|(key, _)| *key != "type").map(|(k, v)| (k.clone(), v.clone())).collect()
        }
        _ => map
            .iter()
            .map(|(name, member)| {
                let mut member = member.clone();
                let fields = member.as_object_mut().unwrap();
                fields.remove("format");
                fields.remove("description");
                (name.clone(), values(&member))
            })
            .collect(),
    }
}

/// A document of `fprime-dp` as `fgds dp decode` writes it, without the names of the record types.
fn document(python: &Value) -> Value {
    let records = python["Records"].as_array().unwrap().iter().map(|record| {
        let mut definition = record["Record"].clone();
        definition.as_object_mut().unwrap().retain(|key, _| !key.starts_with("record_type"));
        serde_json::json!({"Record": definition, "Data": values(&record["Data"])})
    });
    serde_json::json!({"Header": values(&python["Header"]), "Records": records.collect::<Vec<_>>()})
}

#[test]
fn validate_gives_the_verdict_and_the_text_of_fprime_dp() {
    let mut runs = 0;
    for (dir, expected, bytes, dictionary) in products() {
        let from_dictionary = dictionary.header_size().unwrap();
        assert_eq!(expected["header_size"], from_dictionary, "{}", dir.display());
        for run in expected["runs"].as_array().unwrap() {
            let mode = run["mode"].as_str().unwrap();
            if !mode.starts_with("validate") {
                continue;
            }
            let args: Vec<&str> = run["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
            let given = args.iter().position(|a| *a == "-s").map(|i| args[i + 1].parse().unwrap());
            let header_size = match (given, args.contains(&"-d")) {
                (Some(size), _) => HeaderSize::Given(size),
                (None, true) => HeaderSize::Dictionary(from_dictionary),
                (None, false) => HeaderSize::Guess,
            };
            let validation = validate(&bytes, header_size);
            let mut stdout = String::new();
            if args.contains(&"-v") {
                validation.verbose.iter().for_each(|line| stdout += &format!("{line}\n"));
            }
            let (code, stderr) = match validation.result {
                Ok(()) => {
                    stdout += "Validation OK!\n";
                    (0, String::new())
                }
                Err(Invalid::Failed(message)) => (1, format!("{message}\n")),
                Err(Invalid::Crash(reason)) => panic!("{} {mode}: {reason}", dir.display()),
            };
            let ours = (code, stdout, stderr);
            let theirs =
                (run["exit_code"].as_i64().unwrap(), run["stdout"].as_str().unwrap(), run["stderr"].as_str().unwrap());
            assert_eq!((ours.0, ours.1.as_str(), ours.2.as_str()), theirs, "{} {mode}", dir.display());
            runs += 1;
        }
    }
    assert_eq!(runs, 18 * 8);
}
