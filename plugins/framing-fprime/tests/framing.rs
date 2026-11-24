use bytes::BytesMut;
use fgds_core::{Deframed, FramingLayer, PeerInfo, PluginConfig, Registry};
use framing_fprime::FprimeFraming;

fn frame(packet: &[u8]) -> Vec<u8> {
    let mut out = BytesMut::new();
    // The largest frame of the framing itself, not the 2048 bytes of stock flight software.
    let mut cfg = PluginConfig::default();
    cfg.0.insert("flight-frame-size".into(), "4107".into());
    FprimeFraming::default().framer(&cfg).frame(packet, &Default::default(), &mut out).unwrap();
    out.to_vec()
}

/// Feed the input in pieces of `step` bytes; return (packets, discarded bytes, reasons).
fn deframe(input: &[u8], step: usize) -> (Vec<Vec<u8>>, Vec<u8>, Vec<String>) {
    let mut deframer = FprimeFraming::default().deframer(&PluginConfig::default(), &PeerInfo::default());
    let (mut buf, mut out) = (BytesMut::new(), Vec::new());
    for piece in input.chunks(step) {
        buf.extend_from_slice(piece);
        deframer.deframe(&mut buf, &mut out).unwrap();
    }
    let (mut packets, mut discarded, mut reasons) = (Vec::new(), Vec::new(), Vec::new());
    for item in out {
        match item {
            Deframed::Packet { bytes, meta } => {
                assert!(meta.0.is_empty());
                packets.push(bytes.to_vec());
            }
            Deframed::Discarded { bytes, reason } => {
                discarded.extend_from_slice(&bytes);
                reasons.push(reason);
            }
        }
    }
    (packets, discarded, reasons)
}

#[test]
fn frame_has_start_word_length_packet_and_crc32() {
    // CRC-32 of DE AD BE EF 00 00 00 03 01 02 03, as zlib.crc32 computes it.
    let expected = [0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 3, 1, 2, 3, 0x08, 0x16, 0x29, 0x13];
    assert_eq!(frame(&[1, 2, 3]), expected);
    let (packets, discarded, _) = deframe(&frame(&[1, 2, 3]), 15);
    assert_eq!((packets, discarded), (vec![vec![1, 2, 3]], vec![]));
}

#[test]
fn frames_are_found_whatever_the_read_boundaries() {
    let mut input = frame(b"first");
    input.extend(frame(b""));
    input.extend(frame(&[0xDE, 0xAD, 0xBE, 0xEF]));
    for step in [1, 2, 3, 7, input.len()] {
        let (packets, discarded, _) = deframe(&input, step);
        assert_eq!(packets, [b"first".to_vec(), vec![], vec![0xDE, 0xAD, 0xBE, 0xEF]], "step {step}");
        assert!(discarded.is_empty());
    }
}

#[test]
fn bytes_outside_frames_are_discarded_and_framing_recovers() {
    let mut input = b"junk".to_vec();
    input.extend(frame(b"one"));
    input.extend([0xDE, 0xAD, 0x00]);
    input.extend(frame(b"two"));
    for step in [1, 5, input.len()] {
        let (packets, discarded, reasons) = deframe(&input, step);
        assert_eq!(packets, [b"one".to_vec(), b"two".to_vec()], "step {step}");
        assert_eq!(discarded, b"junk\xDE\xAD\x00");
        assert!(reasons.iter().all(|r| *r == "no start word"));
    }
}

#[test]
fn a_corrupted_frame_is_discarded_and_the_next_one_is_kept() {
    let mut bad = frame(b"bad");
    bad[9] ^= 0xFF;
    let mut input = bad.clone();
    input.extend(frame(b"good"));
    let (packets, discarded, reasons) = deframe(&input, input.len());
    assert_eq!(packets, [b"good".to_vec()]);
    assert_eq!(discarded, bad);
    assert_eq!(reasons[0], "checksum mismatch");
}

#[test]
fn an_impossible_length_is_not_waited_for() {
    let mut input = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xFF, 0xFF, 0xFF, 0xFF];
    input.extend(frame(b"ok"));
    let (packets, discarded, _) = deframe(&input, input.len());
    assert_eq!(packets, [b"ok".to_vec()]);
    assert_eq!(discarded.len(), 8);
}

#[test]
fn plugin_registers_layer_and_profile() {
    let mut registry = Registry::new();
    registry.register(&framing_fprime::FprimeFramingPlugin).unwrap();
    assert_eq!(registry.framing_profiles["fprime"], ["fprime"]);
    registry.check().unwrap();
}

#[test]
fn the_largest_packet_is_4095_bytes_in_both_directions() {
    let largest = vec![7u8; 4095];
    let framed = frame(&largest);
    assert_eq!(deframe(&framed, 1000).0, [largest]);
    // One byte more would be taken for noise by the receiver: refused when sending.
    let mut out = BytesMut::new();
    let refused =
        FprimeFraming::default().framer(&PluginConfig::default()).frame(&[7u8; 4096], &Default::default(), &mut out);
    assert!(matches!(refused, Err(fgds_core::Error::Invalid(_))), "{refused:?}");
    assert!(out.is_empty());
    // A received length of 4,096 is not a frame.
    let mut input = vec![0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0x10, 0];
    input.extend(frame(b"ok"));
    let (packets, discarded, _) = deframe(&input, 3);
    assert_eq!((packets, discarded.len()), (vec![b"ok".to_vec()], 8));
}

#[test]
fn a_start_word_split_across_reads_and_a_false_start_are_handled() {
    let mut input = vec![0xDE, 0xAD, 0xDE, 0xAD, 0xBE];
    input.extend(frame(b"one"));
    for step in 1..=7 {
        let (packets, discarded, _) = deframe(&input, step);
        assert_eq!((packets, discarded), (vec![b"one".to_vec()], vec![0xDE, 0xAD, 0xDE, 0xAD, 0xBE]), "step {step}");
    }
}

#[test]
fn a_frame_larger_than_the_flight_software_accepts_is_not_sent() {
    let framer = |size: Option<&str>| {
        let mut cfg = PluginConfig::default();
        if let Some(size) = size {
            cfg.0.insert("flight-frame-size".into(), size.into());
        }
        FprimeFraming::default().framer(&cfg)
    };
    let sent =
        |size, packet: usize| framer(size).frame(&vec![0; packet], &Default::default(), &mut BytesMut::new()).is_ok();
    // Stock: frames of 2048 bytes, 12 of them around the packet.
    assert!(sent(None, 2036) && !sent(None, 2037));
    assert!(sent(Some("100"), 88) && !sent(Some("100"), 89));
    assert!(sent(Some("4107"), 4095) && !sent(Some("4107"), 4096));
}

#[test]
fn the_flight_frame_size_comes_from_the_dictionary_when_it_holds_the_constant() {
    let text = include_str!("../../../tests/data/dictionary.json");
    let mut json: fgds_core::serde_json::Value = fgds_core::serde_json::from_str(text).unwrap();
    let stock = fgds_core::Dictionary::parse("dictionary.json", text).unwrap();
    assert!(FprimeFraming::default().with_dictionary(&stock).is_none());
    assert!(FprimeFraming::default()
        .limit(&PluginConfig::default())
        .unwrap()
        .contains("2048 bytes, frameAccumulatorSize of ComFprime"));

    let constant = fgds_core::serde_json::json!({
        "kind": "constant", "qualifiedName": "ComFprimeConfig.BuffMgr.frameAccumulatorSize",
        "type": {"name": "U64", "kind": "integer", "size": 64, "signed": false}, "value": 1024
    });
    match json["constants"].as_array_mut() {
        Some(constants) => constants.push(constant),
        None => json["constants"] = fgds_core::serde_json::json!([constant]),
    }
    let dictionary = fgds_core::Dictionary::parse("dictionary.json", &json.to_string()).unwrap();
    let layer = FprimeFraming::default().with_dictionary(&dictionary).unwrap();
    let sent = |cfg: &PluginConfig, packet: usize| {
        layer.framer(cfg).frame(&vec![0; packet], &Default::default(), &mut BytesMut::new()).is_ok()
    };
    let mut cfg = PluginConfig::default();
    assert!(sent(&cfg, 1012) && !sent(&cfg, 1013));
    assert!(layer.limit(&cfg).unwrap().contains("1024 bytes"));
    // The option, when given, is what counts.
    cfg.0.insert("flight-frame-size".into(), "2048".into());
    assert!(sent(&cfg, 2036) && !sent(&cfg, 2037));
}
