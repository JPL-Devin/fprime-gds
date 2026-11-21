use fgds_core::serde_json::json;
use fgds_core::testing::FakeGds;
use fgds_core::Dictionary;

const DICTIONARY: &str = include_str!("../../../tests/data/dictionary.json");
const TIME: [u8; 11] = [0, 2, 0, 0, 0, 0, 100, 0, 0, 0, 7];

async fn gds() -> FakeGds {
    let dictionary = Dictionary::parse("dictionary.json", DICTIONARY).unwrap();
    FakeGds::start(&[&channels::ChannelsPlugin], dictionary).await.unwrap()
}

fn channel(id: u16, value: &[u8]) -> Vec<u8> {
    [&[0, 1, 0, 0][..], &id.to_be_bytes(), &TIME, value].concat()
}

#[tokio::test]
async fn channel_packets_become_samples() {
    let gds = gds().await;
    gds.receive(&channel(768, &[0, 0, 1, 0])).await;
    gds.receive(&channel(769, &21.5f32.to_be_bytes())).await;
    gds.receive(&channel(770, &[0, 2, b'o', b'k'])).await;
    gds.receive(&channel(999, &[])).await;
    gds.receive(&channel(768, &[0, 0, 1])).await;

    let samples = gds.records(&["channel"]).await;
    let bodies: Vec<_> = samples.iter().map(|r| r.body.clone()).collect();
    assert_eq!(
        bodies,
        [
            json!({"name": "T.c.Count", "value": 256, "display_text": "256"}),
            json!({"name": "T.c.Temp", "value": 21.5, "display_text": "21.5 C"}),
            json!({"name": "T.c.Label", "value": "ok", "display_text": "ok"}),
        ]
    );
    assert_eq!(samples[0].id, Some(768));
    assert_eq!(samples[0].flight_time.unwrap().seconds, 100);
    assert_eq!(gds.records(&["gds.decode_error"]).await.len(), 2);
}

#[tokio::test]
async fn a_telemetry_packet_becomes_one_sample_per_channel() {
    let gds = gds().await;
    // Packet 1: Label in a slot of 2 + 8 bytes, Count, Temp.
    let label = [0, 2, b'h', b'i', 0, 0, 0, 0, 0, 0];
    let values = [&label[..], &[0, 0, 0, 9], &1.25f32.to_be_bytes()].concat();
    let packet = [&[0, 4, 0, 1][..], &TIME, &values].concat();
    gds.receive(&packet).await;
    gds.receive(&[&[0, 4, 0, 7][..], &TIME].concat()).await; // unknown packet
    gds.receive(&packet[..packet.len() - 1]).await; // truncated

    let samples = gds.records(&["channel"]).await;
    let got: Vec<_> = samples.iter().map(|r| (r.id.unwrap(), r.body["value"].clone())).collect();
    assert_eq!(got, [(770, json!("hi")), (768, json!(9)), (769, json!(1.25))]);
    assert_eq!(samples[1].raw.as_ref(), [0, 0, 0, 9]);
    assert!(samples.iter().all(|r| r.flight_time.unwrap().microseconds == 7));

    // Current state: the latest sample of each channel.
    gds.receive(&channel(768, &[0, 0, 0, 10])).await;
    let query = fgds_core::Query { kinds: vec!["channel".into()], ..Default::default() };
    let latest = gds.core.records.latest(query).await.unwrap().records;
    let got: Vec<_> = latest.iter().map(|r| (r.id.unwrap(), r.body["value"].clone())).collect();
    assert_eq!(got, [(770, json!("hi")), (769, json!(1.25)), (768, json!(10))]);

    let errors = gds.records(&["gds.decode_error"]).await;
    assert_eq!(errors[0].body["reason"], "telemetry packet 7 is not in the dictionary");
    assert_eq!(errors.len(), 2);
}
