//! SEQ-1: every sequence of `fixtures/sequences/manifest.json` compiles to the binary of the
//! Python compiler, or fails with the message of the Python compiler.

use fgds_core::serde_json::{self, Value};
use std::path::{Path, PathBuf};

const PYTHON_INPUT_DIR: &str = "<fprime-gds>/test/fprime_gds/common/tools/input";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sequences")
}

fn compile(case: &Value) -> Result<Vec<u8>, String> {
    let input = fixtures().join(case["input"].as_str().unwrap());
    let dict = sequences::load_dictionary(&fixtures().join(case["dictionary"].as_str().unwrap())).unwrap();
    let text = std::fs::read_to_string(&input).unwrap();
    let file = format!("{PYTHON_INPUT_DIR}/{}", input.file_name().unwrap().to_str().unwrap());
    sequences::compile(&text, &file, &dict, 0xFFFF, case["continue_on_error"].as_bool().unwrap())
}

#[test]
fn manifest() {
    let manifest: Value = serde_json::from_slice(&std::fs::read(fixtures().join("manifest.json")).unwrap()).unwrap();
    let cases = manifest["sequences"].as_array().unwrap();
    assert_eq!(cases.len(), 37);
    let mut failures = Vec::new();
    for case in cases {
        let name = case["input"].as_str().unwrap();
        let result = compile(case);
        match (case["result"].as_str().unwrap(), result) {
            ("binary", Ok(binary)) => {
                let expected = std::fs::read(fixtures().join(case["expected_file"].as_str().unwrap())).unwrap();
                assert_eq!(expected.len() as u64, case["size"].as_u64().unwrap(), "{name}");
                if binary != expected {
                    failures.push(format!("{name}: binary differs"));
                }
            }
            ("error", Err(message)) => {
                if message != case["python_error"].as_str().unwrap() {
                    failures.push(format!("{name}:\n  rust:   {message}\n  python: {}", case["python_error"]));
                }
                if let Some(file) = case["expected_file"].as_str() {
                    let expected = std::fs::read_to_string(fixtures().join(file)).unwrap();
                    assert!(message.contains(expected.trim()), "{name}: {message} lacks {expected}");
                }
                // The message names the line: file:line.
                let line =
                    message.split(&format!("{}:", Path::new(name).file_name().unwrap().to_str().unwrap())).nth(1);
                assert!(line.is_some_and(|l| l.starts_with(|c: char| c.is_ascii_digit())), "{name}: {message}");
            }
            (expected, Ok(_)) => failures.push(format!("{name}: compiled, expected {expected}")),
            (expected, Err(message)) => failures.push(format!("{name}: expected {expected}, got error {message}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
