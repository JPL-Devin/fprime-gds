//! Every case of `fixtures/vectors/ccsds.json`: the calls the Python tests make on the
//! Python framers, replayed on the stages of this crate.

use framing_ccsds::stage::{
    deframe_all, Apids, SpaceDataLink, SpacePacket, SpacePacketSplitter, Stage, TmFrameAggregator,
};
use serde_json::Value;

const VECTORS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/vectors/ccsds.json");

/// Python `None` as input (two cases) has no counterpart here: a slice is always there.
/// Those steps run on empty input, and expect what Python returns: nothing.
fn unhex(value: &Value) -> Vec<u8> {
    let text = value.as_str().unwrap_or_default();
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One Python framer: a single stage, or a chain in framing order (innermost first).
struct Instance {
    stages: Vec<Box<dyn Stage>>,
}

fn stage(spec: &Value) -> Box<dyn Stage> {
    let number = |key: &str| spec[key].as_u64();
    match spec["class"].as_str().unwrap() {
        "SpacePacketFramerDeframer" => {
            let values = spec["apid_enum"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).collect();
            Box::new(SpacePacket::new(Apids { width: 2, values: Some(values) }))
        }
        "SpacePacketSplitterFramerDeframer" => Box::new(SpacePacketSplitter),
        "SpaceDataLinkFramerDeframer" => Box::new(SpaceDataLink::new(
            number("scid").unwrap() as u16,
            number("vcid").unwrap() as u8,
            number("frame_size").unwrap() as usize,
        )),
        "TmFrameAggregatorFramerDeframer" => Box::new(TmFrameAggregator {
            frame_size: number("frame_size").unwrap() as usize,
            scid: number("scid").map(|scid| scid as u16),
        }),
        other => panic!("unknown class {other}"),
    }
}

impl Instance {
    fn new(spec: &Value) -> Instance {
        let stages = match spec["frame_order"].as_array() {
            Some(order) => order.iter().map(stage).collect(),
            None => vec![stage(spec)],
        };
        Instance { stages }
    }

    fn frame(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        self.stages.iter_mut().try_fold(data.to_vec(), |data, stage| stage.frame(&data))
    }

    /// `ChainedFramerDeframer.deframe_all`: outermost stage first, each packet of one
    /// stage is the input of the next; the bytes kept are those of the outermost stage.
    fn deframe_all(&mut self, data: &[u8], warnings: &mut Vec<String>) -> (Vec<Vec<u8>>, Vec<u8>, Vec<u8>) {
        let (mut packets, mut kept, mut discarded) = (vec![data.to_vec()], None, Vec::new());
        for stage in self.stages.iter_mut().rev() {
            let mut next = Vec::new();
            for packet in &packets {
                let (found, rest, junk) = deframe_all(stage.as_mut(), packet, warnings);
                kept.get_or_insert(rest);
                discarded.extend_from_slice(&junk);
                next.extend(found);
            }
            packets = next;
        }
        (packets, kept.unwrap_or_default(), discarded)
    }
}

#[test]
fn every_python_vector() {
    let file: Value = serde_json::from_str(&std::fs::read_to_string(VECTORS).unwrap()).unwrap();
    let cases = file["cases"].as_array().unwrap();
    let mut steps = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut instances: Vec<Instance> = case["instances"].as_array().unwrap().iter().map(Instance::new).collect();
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let at = format!("{name} step {i}");
            let instance = &mut instances[step["instance"].as_u64().unwrap() as usize];
            let input = unhex(&step["input"]);
            let expected = &step["expected"];
            let mut warnings = Vec::new();
            match step["operation"].as_str().unwrap() {
                "frame" => match (instance.frame(&input), step.get("expected_error")) {
                    (Ok(frame), None) => assert_eq!(hex(&frame), expected.as_str().unwrap(), "{at}"),
                    (Err(message), Some(error)) => assert_eq!(message, error["message"].as_str().unwrap(), "{at}"),
                    (result, error) => panic!("{at}: {result:?}, expected error {error:?}"),
                },
                "deframe" => {
                    assert_eq!(instance.stages.len(), 1, "{at}: deframe on a chain");
                    let result = instance.stages[0].deframe(&input, &mut warnings);
                    let packet = result.packet.as_deref().map(hex);
                    assert_eq!(packet.as_deref(), expected["packet"].as_str(), "{at}: packet");
                    let leftover = expected["leftover"].as_str().unwrap_or_default();
                    assert_eq!(hex(&input[result.consumed..]), leftover, "{at}: leftover");
                    assert_eq!(hex(&result.discarded), expected["discarded"].as_str().unwrap(), "{at}: discarded");
                }
                "deframe_all" => {
                    let (packets, kept, discarded) = instance.deframe_all(&input, &mut warnings);
                    let packets: Vec<String> = packets.iter().map(|p| hex(p)).collect();
                    let wanted: Vec<&str> =
                        expected["packets"].as_array().unwrap().iter().map(|p| p.as_str().unwrap()).collect();
                    assert_eq!(packets, wanted, "{at}: packets");
                    assert_eq!(hex(&kept), expected["leftover"].as_str().unwrap(), "{at}: leftover");
                    assert_eq!(hex(&discarded), expected["discarded"].as_str().unwrap(), "{at}: discarded");
                }
                other => panic!("{at}: unknown operation {other}"),
            }
            let wanted: Vec<&str> = step["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
            assert_eq!(warnings, wanted, "{at}: warnings");
            steps += 1;
        }
    }
    // The whole file was run, not a part of it.
    assert_eq!((cases.len(), steps), (75, 324));
}
