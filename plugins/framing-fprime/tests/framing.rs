use bytes::BytesMut;
use fgds_core::{Deframed, FramingLayer, PeerInfo, PluginConfig, Registry};
use framing_fprime::FprimeFraming;

fn frame(packet: &[u8]) -> Vec<u8> {
    let mut out = BytesMut::new();
    FprimeFraming.framer(&PluginConfig::default()).frame(packet, &mut out).unwrap();
    out.to_vec()
}

/// Feed the input in pieces of `step` bytes; return (packets, discarded bytes, reasons).
fn deframe(input: &[u8], step: usize) -> (Vec<Vec<u8>>, Vec<u8>, Vec<&'static str>) {
    let mut deframer = FprimeFraming.deframer(&PluginConfig::default(), &PeerInfo::default());
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
