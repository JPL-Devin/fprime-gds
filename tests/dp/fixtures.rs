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
                    let python = std::fs::read_to_string(dir.join(file)).unwrap();
                    let text = decoded.to_json().to_text();
                    let (ours, theirs): (Value, Value) =
                        (serde_json::from_str(&text).unwrap(), serde_json::from_str(&python).unwrap());
                    assert_eq!(ours, theirs, "{context}: values");
                    documents += 1;
                }
                (Err(DpError::Product(message)), None) => {
                    assert_eq!(run["stderr"], format!("Error: {message}\n"), "{context}");
                    assert_eq!(run["exit_code"], 1, "{context}");
                }
                (other, _) => panic!("{context}: {other:?}"),
            }
        }
    }
    assert_eq!(documents, 32);
}

#[test]
fn validate_gives_the_verdict_and_the_text_of_fprime_dp() {
    let mut runs = 0;
    for (dir, expected, bytes, dictionary) in products() {
        let header = dictionary.header_type().unwrap();
        let from_dictionary = dictionary.max_size(&data_products::types::Ty::Struct(header)).unwrap() as usize;
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
