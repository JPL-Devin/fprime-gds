use fgds_core::serde_json::json;
use fgds_core::testing::{FakeGds, LengthFraming};
use fgds_core::{Dictionary, SendError};

const DICTIONARY: &str = include_str!("../../../tests/data/dictionary.json");

async fn gds() -> FakeGds {
    let dictionary = Dictionary::parse("dictionary.json", DICTIONARY).unwrap();
    FakeGds::start(&[&commands::CommandsPlugin], dictionary).await.unwrap()
}

#[tokio::test]
async fn commands_are_encoded_sent_and_recorded() {
    let gds = gds().await;
    let uplink = &gds.core.uplink;
    uplink.send("command", &json!({"name": "T.c.NO_OP"}), "test").await.unwrap();
    let set = json!({"name": "T.c.SET", "args": ["ON", "0x10", "hi"]});
    uplink.send("command", &set, "test").await.unwrap();

    // Packet type 0, opcode, then mode (U8), level (U16), label (length, text).
    let no_op = vec![0, 0, 0, 0, 1, 0];
    let set_packet = vec![0, 0, 0, 0, 1, 1, 1, 0, 16, 0, 2, b'h', b'i'];
    assert_eq!(gds.link.sent(), [LengthFraming::frame(&no_op), LengthFraming::frame(&set_packet)]);

    let records = gds.records(&["command"]).await;
    assert_eq!((records[0].id, records[0].raw.as_ref()), (Some(256), &no_op[..]));
    assert_eq!(records[0].body["outcome"], "sent");
    assert_eq!(records[1].body["name"], "T.c.SET");
    // The record holds what was sent: numbers as numbers, whatever the request used.
    assert_eq!(records[1].body["args"], json!({"mode": "ON", "level": 16, "label": "hi"}));
}

#[tokio::test]
async fn arguments_may_be_given_by_name() {
    let gds = gds().await;
    let by_name = json!({"name": "T.c.SET", "args": {"label": "hi", "level": 16, "mode": "ON"}});
    let listed = json!({"name": "T.c.SET", "args": ["ON", 16, "hi"]});
    let uplink = &gds.core.uplink;
    assert_eq!(uplink.dry_run("command", &by_name).unwrap().packet, uplink.dry_run("command", &listed).unwrap().packet);
    // A dry run sends and records nothing.
    assert!(gds.link.sent().is_empty());
    assert!(gds.records(&["command"]).await.is_empty());
}

#[tokio::test]
async fn invalid_commands_are_rejected_and_recorded_but_not_sent() {
    let gds = gds().await;
    let cases = [
        (json!({}), "\"name\" is missing"),
        (json!({"name": "T.c.NOPE"}), "unknown command T.c.NOPE"),
        (json!({"name": "T.c.NO_OP", "args": [1]}), "T.c.NO_OP takes 0 arguments, 1 given"),
        (json!({"name": "T.c.SET", "args": ["ON", 1]}), "T.c.SET takes 3 arguments, 2 given"),
        (json!({"name": "T.c.SET", "args": {"mode": "ON", "level": 1}}), "argument label is missing"),
        (
            json!({"name": "T.c.SET", "args": {"mode": "ON", "level": 1, "label": "", "x": 1}}),
            "T.c.SET has no argument x",
        ),
        (json!({"name": "T.c.SET", "args": "ON"}), "\"args\" is neither a list nor an object"),
    ];
    for (request, reason) in &cases {
        let result = gds.core.uplink.send("command", request, "test").await;
        assert_eq!(result, Err(SendError::Invalid(reason.to_string())));
    }
    for (args, reason) in [
        (json!(["BLUE", 1, ""]), "argument mode:"),
        (json!(["ON", 65536, ""]), "argument level:"),
        (json!(["ON", 1, "far too long"]), "argument label:"),
    ] {
        let request = json!({"name": "T.c.SET", "args": args});
        match gds.core.uplink.send("command", &request, "test").await {
            Err(SendError::Invalid(message)) => assert!(message.starts_with(reason), "{message}"),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }
    assert!(gds.link.sent().is_empty());
    let records = gds.records(&["command"]).await;
    assert_eq!(records.len(), cases.len() + 3);
    assert!(records.iter().all(|r| r.body["outcome"] == "rejected"));
    assert_eq!(records[1].body["request"], json!({"name": "T.c.NOPE"}));
}
