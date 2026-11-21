//! The index of the watched directory (DP-5..8): what is listed, when, and as what.

use data_products::{DpDictionary, Index, Options, KIND};
use fgds_core::serde_json::{json, Value};
use fgds_core::NewRecord;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dp/test_dp_data")
}

fn product(name: &str) -> Vec<u8> {
    std::fs::read(data_dir().join(name)).unwrap()
}

struct Watched {
    dir: PathBuf,
    index: Index,
    dict: DpDictionary,
    start: Instant,
}

const SETTLE: Duration = Duration::from_secs(2);

impl Watched {
    fn new(name: &str) -> Watched {
        let dir = std::env::temp_dir().join(format!("fgds-dp-index-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let options = Options { dir: Some(dir.clone()), settle: SETTLE, max_bytes: 4096 };
        let dict = DpDictionary::load(data_dir().join("dictionary_ref.json").to_str().unwrap()).unwrap();
        Watched { dir, index: Index::new(options), dict, start: Instant::now() }
    }

    fn write(&self, name: &str, content: &[u8]) {
        std::fs::write(self.dir.join(name), content).unwrap();
    }

    /// Look at the directory `seconds` after the start.
    fn scan_at(&mut self, seconds: f32) -> Vec<NewRecord> {
        self.index.scan(&self.dict, self.start + Duration::from_secs_f32(seconds))
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sources(record: &NewRecord) -> Vec<&str> {
    record.body["sources"].as_array().unwrap().iter().map(|s| s["path"].as_str().unwrap()).collect()
}

#[test]
fn a_file_is_indexed_once_its_size_is_stable_with_its_header_fields() {
    let mut watched = Watched::new("stable");
    let whole = product("DpDemoUncompressed.fdp");
    watched.write("Dp_1.fdp", &whole[..100]);
    assert!(watched.scan_at(0.0).is_empty());
    assert!(watched.scan_at(1.9).is_empty());
    // Still being written: the wait starts again.
    watched.write("Dp_1.fdp", &whole);
    assert!(watched.scan_at(2.1).is_empty());
    assert!(watched.scan_at(4.0).is_empty());
    let records = watched.scan_at(4.2);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.kind, KIND);
    // The entity is the SHA-256 of the content, as recorded with the fixture.
    assert_eq!(record.entity.as_deref(), Some("05ec5ec88e1acca0c121a4cf241d2c04fee5f7220ffcead02c1c9f238e5490a6"));
    assert_eq!(record.body["hash"], "05ec5ec88e1acca0c121a4cf241d2c04fee5f7220ffcead02c1c9f238e5490a6");
    assert_eq!(record.id, Some(2576));
    assert_eq!(record.body["container"], "Ref.dpDemo.DpDemoContainer");
    assert_eq!(record.body["priority"], 1);
    assert_eq!(record.body["size"], whole.len());
    assert_eq!(record.body["data_size"], 1212);
    assert_eq!(record.body["records"], 14);
    assert_eq!(record.body["dp_state"], "UNTRANSMITTED");
    assert_eq!(
        (&record.body["valid"], &record.body["error"], &record.body["missing"]),
        (&json!(true), &Value::Null, &json!(false))
    );
    assert_eq!(record.flight_time.unwrap().seconds, record.body["time"]["seconds"].as_u64().unwrap() as u32);
    assert_eq!(sources(record), ["Dp_1.fdp"]);
    // Nothing changed: nothing is published again.
    assert!(watched.scan_at(10.0).is_empty());
}

#[test]
fn equal_content_is_one_product_and_a_product_without_file_is_missing() {
    let mut watched = Watched::new("dedup");
    let content = product("DpDemoCompressed.fdp");
    watched.write("a.fdp", &content);
    std::fs::create_dir(watched.dir.join("sub")).unwrap();
    watched.write("sub/b.fdp", &content);
    watched.scan_at(0.0);
    let records = watched.scan_at(2.0);
    assert_eq!(records.len(), 1);
    assert_eq!(sources(&records[0]), ["a.fdp", "sub/b.fdp"]);
    let hash = records[0].entity.clone();

    std::fs::remove_file(watched.dir.join("a.fdp")).unwrap();
    let records = watched.scan_at(3.0);
    assert_eq!(
        (records.len(), sources(&records[0]), &records[0].body["missing"]),
        (1, vec!["sub/b.fdp"], &json!(false))
    );

    std::fs::remove_file(watched.dir.join("sub/b.fdp")).unwrap();
    let records = watched.scan_at(4.0);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].entity, hash);
    assert_eq!(records[0].body["missing"], true);
    assert!(sources(&records[0]).is_empty());
    // The header fields of the missing product are still listed.
    assert_eq!(records[0].body["container"], "Ref.dpDemo.DpDemoContainer");

    // The file comes back: the same entity is no longer missing.
    watched.write("c.fdp", &content);
    watched.scan_at(5.0);
    let records = watched.scan_at(7.0);
    assert_eq!(
        (records[0].entity.clone(), &records[0].body["missing"], sources(&records[0])),
        (hash, &json!(false), vec!["c.fdp"])
    );
}

#[test]
fn a_changed_file_is_indexed_again_and_its_old_content_is_missing() {
    let mut watched = Watched::new("changed");
    watched.write("p.fdp", &product("DpDemoCompressed.fdp"));
    watched.scan_at(0.0);
    let first = watched.scan_at(2.0);
    watched.write("p.fdp", &product("DpDemoUncompressed.fdp"));
    assert!(watched.scan_at(3.0).is_empty());
    let mut records = watched.scan_at(5.0);
    records.sort_by_key(|record| record.body["missing"].as_bool());
    assert_eq!(records.len(), 2);
    assert_eq!((records[0].body["data_size"].clone(), sources(&records[0])), (json!(1212), vec!["p.fdp"]));
    assert_eq!((records[1].entity.clone(), &records[1].body["missing"]), (first[0].entity.clone(), &json!(true)));
}

#[test]
fn products_that_fail_validation_are_listed_with_the_error() {
    let mut watched = Watched::new("invalid");
    let good = product("DpDemoUncompressed.fdp");
    let mut header_checksum = good.clone();
    header_checksum[2] ^= 0xff;
    let mut data_checksum = good.clone();
    let last = data_checksum.len() - 1;
    data_checksum[last] ^= 0xff;
    let mut unknown_container = good.clone();
    unknown_container[5] ^= 0x01;
    let header = 61;
    let checksum = crc32(&unknown_container[..header - 4]);
    unknown_container[header - 4..header].copy_from_slice(&checksum.to_be_bytes());
    watched.write("header.fdp", &header_checksum);
    watched.write("data.fdp", &data_checksum);
    watched.write("truncated.fdp", &good[..good.len() - 30]);
    watched.write("short.fdp", &good[..20]);
    watched.write("container.fdp", &unknown_container);
    watched.write("large.fdp", &vec![0u8; 5000]);
    watched.write("other-dictionary.fdp", &product("makeU32.bin"));
    watched.scan_at(0.0);
    let records = watched.scan_at(2.0);
    assert_eq!(records.len(), 7);
    let error = |name: &str| {
        let record = records.iter().find(|record| record.body["name"] == name).unwrap();
        assert_eq!(record.body["valid"], false, "{name}");
        record.body["error"].as_str().unwrap().to_string()
    };
    assert!(error("header.fdp").starts_with("CRC mismatch in Header: expected 0x"));
    assert!(error("data.fdp").starts_with("Invalid Data checksum."));
    assert!(error("truncated.fdp").starts_with("Invalid Data checksum."));
    assert!(error("short.fdp").starts_with("Truncated or unreadable header:"));
    assert_eq!(error("container.fdp"), "Unknown container: id 2577 is not in the dictionary");
    assert_eq!(error("large.fdp"), "Data product is over the size limit of 4096 bytes");
    // A product of another deployment: its header does not fit this dictionary.
    assert!(!error("other-dictionary.fdp").is_empty());
}

#[cfg(unix)]
#[test]
fn symbolic_links_and_other_files_are_not_indexed() {
    let mut watched = Watched::new("links");
    watched.write("real.fdp", &product("DpDemoUncompressed.fdp"));
    watched.write("notes.txt", b"not a product");
    std::os::unix::fs::symlink(data_dir().join("DpDemoCompressed.fdp"), watched.dir.join("link.fdp")).unwrap();
    std::os::unix::fs::symlink(data_dir(), watched.dir.join("linked-dir")).unwrap();
    watched.scan_at(0.0);
    let records = watched.scan_at(2.0);
    assert_eq!(records.len(), 1);
    assert_eq!(sources(&records[0]), ["real.fdp"]);
}

fn crc32(data: &[u8]) -> u32 {
    data_products::decode::crc32(data)
}
