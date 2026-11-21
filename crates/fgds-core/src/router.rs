use crate::{
    Decoder, NewRecord, PacketMatch, PacketMeta, PacketType, KIND_DECODE_ERROR,
    KIND_UNKNOWN_PACKET,
};
use bytes::Bytes;
use fgds_dictionary::{Dictionary, Reader, Type};
use serde_json::json;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// Picks the decoder for each packet from the decoders' match rules. A rule with more
/// conditions wins: metadata conditions first, then a packet type.
pub struct Router {
    /// Most specific rule first.
    rules: Vec<(PacketMatch, Arc<dyn Decoder>)>,
    descriptor_bits: u8,
}

impl Router {
    pub fn new(decoders: &[(String, Arc<dyn Decoder>)], dict: &Dictionary) -> Router {
        let mut rules: Vec<_> = decoders
            .iter()
            .map(|(_, d)| (d.matches(), d.clone()))
            .collect();
        rules.sort_by_key(|(rule, _)| {
            std::cmp::Reverse((rule.meta.len(), rule.packet_type.is_some()))
        });
        let descriptor_bits = match dict.config_type("FwPacketDescriptorType", Type::U16) {
            Type::Int { bits, .. } => bits,
            _ => 16,
        };
        Router {
            rules,
            descriptor_bits,
        }
    }

    /// Turn one packet into records. Never fails: a packet no rule matches becomes
    /// `gds.unknown_packet`, and a decoder error or panic becomes `gds.decode_error` (LNK-8).
    pub fn route(&self, packet: &Bytes, meta: &PacketMeta, dict: &Dictionary) -> Vec<NewRecord> {
        let packet_type = Reader::new(packet)
            .uint(self.descriptor_bits)
            .ok()
            .map(|t| PacketType(t as u32));
        let problem = |kind: &str, reason: String| NewRecord {
            raw: packet.clone(),
            meta: meta.clone(),
            ..NewRecord::new(kind, json!({"reason": reason, "packet_type": packet_type.map(|t| t.0)}))
        };
        let matches = |rule: &PacketMatch| {
            rule.packet_type.is_none_or(|t| Some(t) == packet_type)
                && rule.meta.iter().all(|(key, value)| meta.get(key) == Some(value))
        };
        let Some((_, decoder)) = self.rules.iter().find(|(rule, _)| matches(rule)) else {
            return vec![problem(KIND_UNKNOWN_PACKET, "no decoder matches".into())];
        };
        match catch_unwind(AssertUnwindSafe(|| decoder.decode(packet, meta, dict))) {
            Ok(Ok(mut records)) => {
                for record in &mut records {
                    if record.raw.is_empty() {
                        record.raw = packet.clone();
                    }
                    if record.meta.0.is_empty() {
                        record.meta = meta.clone();
                    }
                }
                records
            }
            Ok(Err(e)) => vec![problem(KIND_DECODE_ERROR, e.0)],
            Err(panic) => vec![problem(
                KIND_DECODE_ERROR,
                format!("decoder panicked: {}", crate::supervisor::panic_message(&panic)),
            )],
        }
    }
}
