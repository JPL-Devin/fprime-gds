//! Fuzz-style test (MNT-6): truncated and corrupted products, and damaged dictionaries,
//! give a result or an error and never a panic. Inputs are derived from the fixtures
//! with a fixed generator, so a failure can be reproduced.

use data_products::decode::{decode, read_header, DecodeOptions};
use data_products::validate::{validate, HeaderSize};
use data_products::{DpDictionary, Index, Options};
use fgds_core::serde_json::{self, Value};
use std::path::{Path, PathBuf};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dp/test_dp_data")
}

/// xorshift64: the same inputs on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Each product with the dictionary that describes it.
fn products() -> Vec<(String, Vec<u8>, &'static str)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(data_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let dictionary = match path.extension().and_then(|e| e.to_str()) {
            Some("bin") => "dictionary.json",
            Some("fdp") => "dictionary_ref.json",
            _ => continue,
        };
        found.push((name, std::fs::read(&path).unwrap(), dictionary));
    }
    assert_eq!(found.len(), 18);
    found
}

fn dictionary(name: &str) -> DpDictionary {
    DpDictionary::load(data_dir().join(name).to_str().unwrap()).unwrap()
}

/// Everything the library does with the bytes of a file.
fn exercise(dict: &DpDictionary, data: &[u8]) -> bool {
    let _ = read_header(dict, data);
    for size in [HeaderSize::Guess, HeaderSize::Dictionary(63), HeaderSize::Given(5), HeaderSize::Given(usize::MAX)] {
        let _ = validate(data, size);
    }
    let _ = decode(dict, data, DecodeOptions { decompress: false, ..Default::default() });
    // A small limit also bounds what a damaged compressed product may inflate to.
    decode(dict, data, DecodeOptions { decompress: true, max_bytes: 1 << 20 }).is_ok()
}

#[test]
fn truncated_products_never_panic_and_never_decode() {
    for (name, data, dict) in products() {
        let dict = dictionary(dict);
        let whole = exercise(&dict, &data);
        assert_eq!(whole, !name.starts_with("CRC_"), "{name}");
        for length in 0..data.len() {
            assert!(!exercise(&dict, &data[..length]), "{name} cut to {length} bytes decoded");
        }
    }
}

#[test]
fn corrupted_products_never_panic() {
    let mut random = Random(0x5eed_f00d_cafe_d00d);
    let mut cases = 0;
    for (_, data, dict) in products() {
        let dict = dictionary(dict);
        // Every byte inverted in turn, then every byte replaced by a random value.
        for at in 0..data.len() {
            for replacement in [!data[at], random.next() as u8] {
                let mut damaged = data.clone();
                damaged[at] = replacement;
                exercise(&dict, &damaged);
                cases += 1;
            }
        }
        // Several changes at once: overwritten runs, removed and inserted bytes.
        for _ in 0..300 {
            let mut damaged = data.clone();
            for _ in 0..1 + random.below(4) {
                let at = random.below(damaged.len());
                match random.below(3) {
                    0 => {
                        let end = (at + 1 + random.below(16)).min(damaged.len());
                        damaged[at..end].iter_mut().for_each(|byte| *byte = random.next() as u8);
                    }
                    1 => drop(damaged.drain(at..(at + 1 + random.below(8)).min(damaged.len()))),
                    _ => (0..1 + random.below(8)).for_each(|_| damaged.insert(at, random.next() as u8)),
                }
            }
            exercise(&dict, &damaged);
            cases += 1;
        }
    }
    // Bytes that were never a product, with every dictionary.
    for dict in ["dictionary.json", "dictionary_ref.json"] {
        let dict = dictionary(dict);
        for _ in 0..2000 {
            let noise: Vec<u8> = (0..random.below(400)).map(|_| random.next() as u8).collect();
            exercise(&dict, &noise);
            cases += 1;
        }
    }
    assert!(cases > 60_000, "{cases} cases");
}

/// Replace one value somewhere inside the type definitions, records or constants.
fn damage(value: &mut Value, random: &mut Random) {
    match value {
        Value::Array(items) if !items.is_empty() => {
            let at = random.below(items.len());
            damage(&mut items[at], random)
        }
        Value::Object(members) if !members.is_empty() => {
            let at = random.below(members.len());
            damage(members.values_mut().nth(at).unwrap(), random)
        }
        other => {
            *other = match random.below(6) {
                0 => Value::Null,
                1 => Value::from(u64::MAX),
                2 => Value::from(-1),
                3 => Value::from("Svc.DpTool.Complex"),
                4 => serde_json::json!({"name": "string", "kind": "string", "size": u64::MAX}),
                _ => serde_json::json!([]),
            }
        }
    }
}

#[test]
fn damaged_dictionaries_never_panic() {
    let mut random = Random(0x0dd_ba11);
    for (name, product) in [("dictionary.json", "makeComplex.bin"), ("dictionary_ref.json", "DpDemoCompressed.fdp")] {
        let json: Value = serde_json::from_slice(&std::fs::read(data_dir().join(name)).unwrap()).unwrap();
        let data = std::fs::read(data_dir().join(product)).unwrap();
        for _ in 0..400 {
            let mut damaged = json.clone();
            for section in ["typeDefinitions", "records", "constants", "containers"] {
                for _ in 0..1 + random.below(3) {
                    damage(&mut damaged[section], &mut random);
                }
            }
            if let Ok(dict) = DpDictionary::from_json(&damaged) {
                exercise(&dict, &data);
            }
        }
    }
    // A type that contains itself is an error, not a stack overflow.
    let recursive = serde_json::json!({"typeDefinitions": [
        {"kind": "array", "qualifiedName": "A", "size": 2, "elementType": {"name": "A", "kind": "qualifiedIdentifier"}}
    ]});
    let dict = DpDictionary::from_json(&recursive).unwrap();
    assert!(dict.get_type("A").unwrap_err().contains("contains itself"));
}

#[test]
fn the_index_never_panics_on_damaged_files() {
    let dir = std::env::temp_dir().join(format!("fgds-dp-robustness-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut random = Random(42);
    for (name, data, _) in products() {
        for copy in 0..4 {
            let mut damaged = data[..random.below(data.len() + 1)].to_vec();
            if let Some(byte) = damaged.last_mut() {
                *byte ^= 0xff;
            }
            std::fs::write(dir.join(format!("{name}-{copy}.fdp")), damaged).unwrap();
        }
    }
    let options = Options { dir: Some(dir.clone()), settle: std::time::Duration::ZERO, max_bytes: 1 << 20 };
    let records = Index::new(options).scan(&dictionary("dictionary_ref.json"), std::time::Instant::now());
    assert!(!records.is_empty());
    assert!(records.iter().all(|record| record.body["valid"] == false && record.body["error"].is_string()));
    std::fs::remove_dir_all(&dir).unwrap();
}
