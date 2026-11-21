use fgds_dictionary::{Dictionary, Member, Reader, Type};
use serde_json::{json, Value};

const DICT: &str = include_str!("data/small.json");

fn dict() -> Dictionary {
    Dictionary::parse("small.json", DICT).unwrap()
}

fn round_trip(d: &Dictionary, ty: &Type, value: Value, bytes: &[u8]) {
    let mut out = Vec::new();
    d.encode(ty, &value, &mut out).unwrap();
    assert_eq!(out, bytes, "encoding of {value}");
    let mut r = Reader::new(bytes);
    assert_eq!(d.decode(ty, &mut r).unwrap(), value);
    assert_eq!(r.remaining(), 0);
}

#[test]
fn loads_definitions() {
    let d = dict();
    assert_eq!(d.version(), "v1");
    let cmd = d.command("T.c.SET").unwrap();
    assert_eq!(cmd.opcode, 0x10);
    assert_eq!(cmd.params.len(), 2);
    assert_eq!(d.command_by_opcode(0x10).unwrap().name, "T.c.SET");
    assert_eq!(d.event(2).unwrap().severity, "ACTIVITY_HI");
    assert_eq!(d.channel(3).unwrap().format.as_deref(), Some("{.2f} V"));
    // An alias resolves to its underlying type.
    assert_eq!(d.types["FwOpcodeType"], Type::U32);
}

#[test]
fn scalar_round_trips() {
    let d = dict();
    let int = |bits, signed| Type::Int { bits, signed };
    round_trip(&d, &int(8, false), json!(255), &[0xFF]);
    round_trip(&d, &int(8, true), json!(-1), &[0xFF]);
    round_trip(&d, &int(16, true), json!(-2), &[0xFF, 0xFE]);
    round_trip(&d, &int(32, false), json!(0xDEADBEEFu32), &[0xDE, 0xAD, 0xBE, 0xEF]);
    // 64-bit integers are decimal strings (API-4).
    round_trip(&d, &int(64, false), json!("18446744073709551615"), &[0xFF; 8]);
    round_trip(&d, &int(64, true), json!("-1"), &[0xFF; 8]);
    round_trip(&d, &Type::Float { bits: 32 }, json!(1.5), &[0x3F, 0xC0, 0, 0]);
    round_trip(&d, &Type::Float { bits: 64 }, json!(-2.0), &[0xC0, 0, 0, 0, 0, 0, 0, 0]);
    round_trip(&d, &Type::Float { bits: 32 }, json!("Infinity"), &[0x7F, 0x80, 0, 0]);
    round_trip(&d, &Type::Bool, json!(true), &[0xFF]);
    round_trip(&d, &Type::Bool, json!(false), &[0x00]);
    round_trip(&d, &Type::String { max: 8 }, json!("hé"), &[0, 3, b'h', 0xC3, 0xA9]);
}

#[test]
fn composite_round_trips() {
    let d = dict();
    round_trip(&d, &d.types["T.Color"], json!("GREEN"), &[1]);
    round_trip(&d, &d.types["T.Pair"], json!([1, 2]), &[0, 1, 0, 2]);
    // Members are encoded by "index": `b` (index 0) before `a` (index 1).
    round_trip(&d, &d.types["T.Mixed"], json!({"a": "RED", "b": [7, 8], "c": [1, 2, 3]}), &[0, 7, 0, 8, 0, 1, 2, 3]);
}

#[test]
fn numbers_may_be_given_as_text() {
    let d = dict();
    let mut out = Vec::new();
    d.encode(&Type::U16, &json!("0x1F"), &mut out).unwrap();
    d.encode(&Type::Bool, &json!("True"), &mut out).unwrap();
    d.encode(&Type::Float { bits: 32 }, &json!("1.5"), &mut out).unwrap();
    assert_eq!(out, [0, 0x1F, 0xFF, 0x3F, 0xC0, 0, 0]);
}

#[test]
fn invalid_values_are_rejected() {
    let d = dict();
    let reject = |ty: &Type, value: Value, part: &str| {
        let e = d.encode(ty, &value, &mut Vec::new()).unwrap_err();
        assert!(e.0.contains(part), "{e} should contain {part}");
    };
    reject(&Type::U8, json!(256), "out of range");
    reject(&Type::U8, json!(-1), "out of range");
    reject(&Type::U8, json!(1.5), "not an integer");
    reject(&Type::Bool, json!(1), "not a boolean");
    reject(&Type::String { max: 2 }, json!("abc"), "exceeds maximum");
    reject(&d.types["T.Color"], json!("PINK"), "not a constant");
    reject(&d.types["T.Pair"], json!([1]), "array of 2");
    reject(&d.types["T.Mixed"], json!({"a": "RED", "b": [1, 2]}), "member c is missing");
    reject(&d.types["T.Mixed"], json!({"a": "RED", "b": [1, 2], "c": [1, 2, 3], "z": 1}), "no member z");
}

#[test]
fn invalid_bytes_are_rejected() {
    let d = dict();
    let reject = |ty: &Type, bytes: &[u8], part: &str| {
        let e = d.decode(ty, &mut Reader::new(bytes)).unwrap_err();
        assert!(e.0.contains(part), "{e} should contain {part}");
    };
    reject(&Type::U32, &[1, 2], "needed 4 bytes");
    reject(&Type::Bool, &[7], "invalid boolean");
    reject(&d.types["T.Color"], &[9], "not a constant");
    reject(&Type::String { max: 2 }, &[0, 3, b'a', b'b', b'c'], "exceeds maximum");
    reject(&Type::String { max: 8 }, &[0, 3, b'a'], "needed 3 bytes");
    reject(&Type::String { max: 8 }, &[0, 1, 0xFF], "UTF-8");
}

#[test]
fn errors_name_the_file_and_the_first_problem() {
    let e = Dictionary::load(std::path::Path::new("/nonexistent/d.json")).unwrap_err();
    assert!(e.to_string().starts_with("dictionary /nonexistent/d.json: "));

    let e = Dictionary::parse("bad.json", "{ not json").unwrap_err();
    assert_eq!(e.file, "bad.json");

    let e = Dictionary::parse("bad.json", "{}").unwrap_err();
    assert_eq!(e.to_string(), "dictionary bad.json: missing array \"typeDefinitions\"");

    let mut json: Value = serde_json::from_str(DICT).unwrap();
    json["commands"][0]["formalParams"][0]["type"] = json!({"name": "T.Nope", "kind": "qualifiedIdentifier"});
    let e = Dictionary::parse("bad.json", &json.to_string()).unwrap_err();
    assert_eq!(e.problem, "commands[0]: undefined type T.Nope");

    let mut json: Value = serde_json::from_str(DICT).unwrap();
    json["events"][1]["id"] = json!(1);
    let e = Dictionary::parse("bad.json", &json.to_string()).unwrap_err();
    assert_eq!(e.problem, "events[1]: duplicate event id 1");
}

#[test]
fn a_type_that_contains_itself_is_rejected() {
    let mut json: Value = serde_json::from_str(DICT).unwrap();
    json["typeDefinitions"].as_array_mut().unwrap().push(json!({
        "kind": "array", "qualifiedName": "T.Loop", "size": 2,
        "elementType": {"name": "T.Loop", "kind": "qualifiedIdentifier"}
    }));
    let e = Dictionary::parse("bad.json", &json.to_string()).unwrap_err();
    assert_eq!(e.problem, "type T.Loop: type T.Loop contains itself");
}

#[test]
fn format_strings_are_filled_as_the_python_gds_fills_them() {
    let dict = dict();
    let (u32t, f64t) = (Type::U32, Type::Float { bits: 64 });
    let u64t = Type::Int { bits: 64, signed: false };
    let i32t = Type::Int { bits: 32, signed: true };
    let text = Type::String { max: 10 };
    type Case<'a> = (&'a str, Vec<(&'a Type, Value)>, &'a str);
    let cases: Vec<Case> = vec![
        ("no fields", vec![], "no fields"),
        ("{} and {}", vec![(&u32t, json!(7)), (&text, json!("abc"))], "7 and abc"),
        ("0x{x} {X} {o} {d}", vec![(&u32t, json!(255)); 4], "0xff FF 377 255"),
        ("{.2f}|{f}|{8.3f}", vec![(&f64t, json!(1.5)); 3], "1.50|1.500000|   1.500"),
        ("{e} {.1e}", vec![(&f64t, json!(12345.678)); 2], "1.234568e+04 1.2e+04"),
        ("{c}", vec![(&u32t, json!(65))], "A"),
        ("{}", vec![(&u64t, json!("18446744073709551615"))], "18446744073709551615"),
        ("{} {} {}", vec![(&f64t, json!(2.0)), (&f64t, json!(0.1)), (&f64t, json!("NaN"))], "2.0 0.1 nan"),
        ("{} {}", vec![(&f64t, json!(1e20)), (&f64t, json!(1.5e-7))], "1e+20 1.5e-07"),
        ("{}", vec![(&Type::Bool, json!(true))], "True"),
        ("{{}} {}", vec![(&u32t, json!(1))], "{} 1"),
        ("{} {}", vec![(&u32t, json!(1))], "1 {}"),
        ("{x}", vec![(&text, json!("abc"))], "{x}"),
        // As Python's `format`: sign and magnitude, a real `g`, non-finite values.
        (
            "{x} {o} {04x} {5d}|",
            vec![(&i32t, json!(-255)), (&i32t, json!(-8)), (&i32t, json!(-1)), (&i32t, json!(-42))],
            "-ff -10 -001   -42|",
        ),
        (
            "{g} {g} {.3g} {g}",
            vec![(&f64t, json!(100.0)), (&f64t, json!(1234567.0)), (&f64t, json!(0.000012345)), (&f64t, json!(0.5))],
            "100 1.23457e+06 1.23e-05 0.5",
        ),
        (
            "{f} {.2f} {e} {g} {8.1f}|",
            vec![
                (&f64t, json!("NaN")),
                (&f64t, json!("Infinity")),
                (&f64t, json!("-Infinity")),
                (&f64t, json!("NaN")),
                (&f64t, json!("-Infinity")),
            ],
            "nan inf -inf nan     -inf|",
        ),
    ];
    for (template, values, expected) in cases {
        let values: Vec<_> = values.iter().map(|(t, v)| (*t, v)).collect();
        assert_eq!(dict.format(template, &values), expected, "{template}");
    }
    let array = Type::Array { name: "A".into(), size: 2, element: Box::new(text.clone()), format: None };
    assert_eq!(dict.display(&array, &json!(["a", "b"])), "['a', 'b']");
    let mixed = json!({"a": "RED", "b": [7, 8], "c": [1, 2, 3]});
    let shown = dict.display(&dict.types["T.Mixed"], &mixed);
    assert_eq!(shown, "{'b': [7, 8], 'a': 'RED', 'c': [1, 2, 3]}");
    assert_eq!(dict.max_size(&dict.types["T.Mixed"]), 8);
    assert_eq!(dict.max_size(&Type::String { max: 10 }), 12);
}

/// The FPP-style vectors recorded from the Python GDS (`format_string_template`).
#[test]
fn format_strings_match_the_python_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/vectors/format_strings.json");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let dict = dict();
    let mut checked = 0;
    for vector in fixture["vectors"].as_array().unwrap() {
        let (Some(expected), Some(values)) = (vector["expected"].as_str(), vector["values"].as_array()) else {
            continue;
        };
        if vector["style"] != "fpp" {
            continue;
        }
        let typed: Vec<(Type, &Value)> = values
            .iter()
            .map(|v| match v {
                Value::Number(n) if n.is_f64() => (Type::Float { bits: 64 }, v),
                Value::Number(_) => (Type::Int { bits: 64, signed: true }, v),
                _ => (Type::String { max: 80 }, v),
            })
            .collect();
        let refs: Vec<_> = typed.iter().map(|(t, v)| (t, *v)).collect();
        assert_eq!(dict.format(vector["template"].as_str().unwrap(), &refs), expected, "{vector}");
        checked += 1;
    }
    assert!(checked >= 8, "only {checked} vectors checked");
}

/// Channels of array and struct type: each leaf goes through the format string of its
/// array element or struct member, as `formatted_val` of the Python GDS does.
#[test]
fn element_and_member_format_strings_are_applied_to_channel_values() {
    let dict = dict();
    let hex = Type::Array { name: "A".into(), size: 2, element: Box::new(Type::U32), format: Some("0x{x}".into()) };
    assert_eq!(dict.display_channel(&hex, &json!([255, 16])), "['0xff', '0x10']");
    let member = |name: &str, ty: Type, count, format: Option<&str>| Member {
        name: name.into(),
        ty,
        count,
        format: format.map(str::to_string),
    };
    let members = vec![
        member("ratio", Type::Float { bits: 32 }, None, Some("{.2f} V")),
        member("pair", Type::U32, Some(2), Some("{03d}")),
        member("inner", hex.clone(), None, None),
        member("plain", Type::U32, None, None),
    ];
    let ty = Type::Struct { name: "S".into(), members };
    let value = json!({"ratio": 1.5, "pair": [1, 2], "inner": [1, 2], "plain": 7});
    let expected = "{'ratio': '1.50 V', 'pair': ['001', '002'], 'inner': ['0x1', '0x2'], 'plain': '7'}";
    assert_eq!(dict.display_channel(&ty, &value), expected);
}

#[test]
fn time_tags_are_base_context_seconds_microseconds() {
    let bytes = [0, 2, 9, 0, 0, 0, 5, 0, 0, 0, 7, 0xFF];
    let mut r = Reader::new(&bytes);
    let time = dict().decode_time(&mut r).unwrap();
    assert_eq!((time.base, time.context, time.seconds, time.microseconds), (2, 9, 5, 7));
    assert_eq!(r.remaining(), 1);
    assert!(dict().decode_time(&mut Reader::new(&bytes[..5])).is_err());
}
