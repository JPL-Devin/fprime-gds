use fgds_core::serde_json::json;
use fgds_core::testing::FakeGds;
use fgds_core::{Dictionary, FlightTime};

const DICTIONARY: &str = include_str!("../../../tests/data/dictionary.json");
/// Time tag: base 2, context 0, 100 s, 7 µs.
const TIME: [u8; 11] = [0, 2, 0, 0, 0, 0, 100, 0, 0, 0, 7];

fn event(id: u16, args: &[u8]) -> Vec<u8> {
    let mut packet = vec![0, 2, 0, 0];
    packet.extend(id.to_be_bytes());
    packet.extend(TIME);
    packet.extend(args);
    packet
}

#[tokio::test]
async fn events_are_decoded_with_their_arguments_and_text() {
    let dictionary = Dictionary::parse("dictionary.json", DICTIONARY).unwrap();
    let gds = FakeGds::start(&[&events::EventsPlugin], dictionary).await.unwrap();
    gds.receive(&event(512, &[])).await;
    gds.receive(&event(513, &[1, 0x01, 0xFF])).await;
    gds.receive(&event(999, &[])).await; // not in the dictionary
    gds.receive(&event(513, &[1])).await; // too short
    gds.receive(&event(512, &[0])).await; // too long

    let events = gds.records(&["event"]).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, Some(512));
    assert_eq!(events[0].flight_time, Some(FlightTime { base: 2, context: 0, seconds: 100, microseconds: 7 }));
    assert_eq!(events[0].raw.as_ref(), event(512, &[]));
    assert_eq!(
        events[0].body,
        json!({"name": "T.c.NoOp", "severity": "ACTIVITY_HI", "args": {}, "display_text": "Received a NO-OP command"})
    );
    assert_eq!(
        events[1].body,
        json!({"name": "T.c.ModeSet", "severity": "WARNING_LO", "args": {"mode": "ON", "level": 511},
               "display_text": "Mode ON at level 0x1ff"})
    );

    let errors = gds.records(&["gds.decode_error"]).await;
    let reasons: Vec<_> = errors.iter().map(|r| r.body["reason"].as_str().unwrap()).collect();
    assert_eq!(reasons.len(), 3);
    assert_eq!(reasons[0], "event 999 is not in the dictionary");
    assert!(reasons[1].starts_with("event T.c.ModeSet argument level:"), "{}", reasons[1]);
    assert_eq!(reasons[2], "needed 4 bytes at offset 17, 1 left");
}

#[tokio::test]
async fn several_events_in_one_packet_are_each_a_record() {
    let dictionary = Dictionary::parse("dictionary.json", DICTIONARY).unwrap();
    let gds = FakeGds::start(&[&events::EventsPlugin], dictionary).await.unwrap();
    let mut packet = event(512, &[]);
    packet.extend(&event(513, &[1, 0x01, 0xFF])[2..]);
    gds.receive(&packet).await;

    let events = gds.records(&["event"]).await;
    let ids: Vec<_> = events.iter().map(|r| r.id).collect();
    assert_eq!(ids, [Some(512), Some(513)]);
}
