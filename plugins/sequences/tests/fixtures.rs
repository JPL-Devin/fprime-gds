//! SEQ-1: every valid sequence of `fixtures/sequences/manifest.json` compiles to the binary of
//! `fprime-seqgen`; every other one is refused with a message naming its file and line.

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

/// Every fixture input cut at every character, and with characters replaced, compiles or
/// is rejected with a message naming the file; nothing panics (MNT-6).
#[test]
fn damaged_sequences_never_panic() {
    let dict = sequences::load_dictionary(&fixtures().join("resources/enhanced_dictionary.json")).unwrap();
    let mut inputs: Vec<_> = std::fs::read_dir(fixtures().join("input")).unwrap().map(|e| e.unwrap().path()).collect();
    inputs.sort();
    let (mut compiled, mut rejected) = (0, 0);
    let mut check = |text: &str, all: bool| match sequences::compile(text, "damaged.seq", &dict, 0xFFFF, all) {
        Ok(binary) => {
            assert!(binary.len() >= 15);
            compiled += 1;
        }
        Err(message) => {
            assert!(message.contains("damaged.seq"), "{message}");
            rejected += 1;
        }
    };
    for path in inputs.iter().filter(|p| p.extension().is_some_and(|e| e == "seq")) {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let chars: Vec<char> = text.chars().collect();
        for end in 0..chars.len() {
            check(&chars[..end].iter().collect::<String>(), end % 2 == 0);
        }
        for (index, replacement) in (0..chars.len()).zip("\"[{:,\\\n;]}'.-x9R\0\u{e9}".chars().cycle()) {
            let mut changed = chars.clone();
            changed[index] = replacement;
            check(&changed.iter().collect::<String>(), index % 2 == 0);
        }
    }
    assert!(compiled > 1000 && rejected > 1000, "{compiled} compiled, {rejected} rejected");
}

/// A statement with fewer or more arguments than its command takes is refused, naming the line and the
/// command, as a command that is sent (the Python compiler writes such a sequence).
#[test]
fn argument_count() {
    let dict = sequences::load_dictionary(&fixtures().join("resources/enhanced_dictionary.json")).unwrap();
    let compile = |text: &str, all: bool| sequences::compile(text, "s.seq", &dict, 0xFFFF, all);
    let text = "R00:00:00 cmdDisp.CMD_NO_OP\nR00:00:01 cmdDisp.CMD_NO_OP 1, 2\nR00:00:02 cmdDisp.CMD_STRING\n";
    assert_eq!(compile(text, false).unwrap_err(), "s.seq:2: cmdDisp.CMD_NO_OP takes 0 arguments, 2 given");
    assert_eq!(
        compile(text, true).unwrap_err(),
        "s.seq:2: cmdDisp.CMD_NO_OP takes 0 arguments, 2 given\ns.seq:3: cmdDisp.CMD_STRING takes 1 arguments, 0 given"
    );
    assert!(compile("R00:00:00 cmdDisp.CMD_NO_OP\nR00:00:02 cmdDisp.CMD_STRING \"a\"\n", false).is_ok());
}

/// A float literal beyond a double is refused, quoted or not: it is not sent as infinity.
#[test]
fn a_float_literal_beyond_a_double_is_refused() {
    let dict = sequences::load_dictionary(&fixtures().join("resources/enhanced_dictionary.json")).unwrap();
    let compile = |text: &str| sequences::compile(text, "s.seq", &dict, 0xFFFF, false);
    for literal in ["1e999", "-1e999", "\"1e400\""] {
        let error = compile(&format!("R00:00:00 cmdDisp.CMD_FLOAT {literal}\n")).unwrap_err();
        assert!(error.starts_with("s.seq:1: cmdDisp.CMD_FLOAT"), "{error}");
    }
    assert!(compile("R00:00:00 cmdDisp.CMD_FLOAT 1e30\n").is_ok());
}

/// The line format: optional commas between arguments, a comment, a continued line, and what is refused.
#[test]
fn line_format() {
    use fgds_core::serde_json::json;
    let parse = |text: &str| sequences::parser::parse(text);
    let statements = parse(
        "; a comment\n\nR00:00:01.5 a.B 1, -0x10 2.5e1 \"t;x\" 'q' NAME true [1, [2, 3],] \\\n  {x: 1, y: \"[4]\", x: 5} ; end\r\nA2025-015T12:00:00 c.D",
    )
    .unwrap();
    assert_eq!(statements.len(), 2);
    let first = &statements[0];
    assert_eq!(
        (first.line, first.relative, first.time.as_str(), first.mnemonic.as_str()),
        (3, true, "00:00:01.5", "a.B")
    );
    let expected = [
        json!(1),
        json!(-16),
        json!(25.0),
        json!("t;x"),
        json!("q"),
        json!("NAME"),
        json!(true),
        json!([1, [2, 3]]),
        json!({"x": 5, "y": [4]}),
    ];
    assert_eq!(first.args, expected);
    assert_eq!(
        (statements[1].line, statements[1].relative, statements[1].time.as_str()),
        (5, false, "2025-015T12:00:00")
    );
    assert_eq!(parse("R00:00:00 a.B 18446744073709551615").unwrap()[0].args, [json!(u64::MAX)]);
    for (text, line, column) in [
        ("R00:00:00 a.B\nX00:00:00 a.B", 2, 1),
        ("R00:00:0 a.B", 1, 9),
        ("R00:00:00 a.B [1 2]", 1, 18),
        ("R00:00:00 a.B \"open", 1, 20),
        ("R00:00:00 a.B 0o17", 1, 15),
        ("R00:00:00 a.B 12ab", 1, 15),
        ("R00:00:00 a.B {\"x\": 1}", 1, 16),
        ("R00:00:00 a.B 1,", 1, 17),
        ("R00:00:00 a.B [1,\n2]", 1, 18),
    ] {
        let error = parse(text).unwrap_err();
        assert_eq!((error.line, error.column), (line, column), "{text}");
    }
}
