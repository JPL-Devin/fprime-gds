//! F Prime events: the decoder of event packets (kind `fprime.event`) and the route that
//! lists the event definitions of the dictionary.

use fgds_core::axum::extract::State;
use fgds_core::axum::routing::get;
use fgds_core::axum::{Json, Router};
use fgds_core::dictionary::Reader;
use fgds_core::serde_json::{json, Map, Value};
use fgds_core::{
    Access, Context, DecodeError, Decoder, Dictionary, FieldSpec, FieldType, FlightTime, KindSpec, Manifest, NewRecord,
    PacketMatch, PacketMeta, PacketType, Plugin, Registry, Result, RouteAccess, Routes,
};
use std::sync::Arc;

pub const KIND: &str = "fprime.event";

pub struct EventsPlugin;

impl Plugin for EventsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "events", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec {
            fields: &[
                FieldSpec { name: "name", pointer: "/name", ty: FieldType::Text },
                FieldSpec { name: "severity", pointer: "/severity", ty: FieldType::Text },
            ],
            text_fields: &["/name", "/display_text"],
            ..KindSpec::new(KIND, "An F Prime event; id is the event identifier")
        })?;
        reg.add_decoder(Arc::new(EventDecoder))?;
        reg.add_routes(Routes {
            prefix: "events",
            router: Router::new().route("/definitions", get(definitions)),
            access: vec![RouteAccess { method: "GET", path: "/definitions", access: Access::Read }],
        })
    }
}

/// The events the dictionary defines.
async fn definitions(State(ctx): State<Context>) -> Json<Value> {
    let events = ctx.dictionary.events.iter().map(|e| {
        let args: Vec<&str> = e.params.iter().map(|p| p.name.as_str()).collect();
        json!({"id": e.id, "name": e.name, "severity": e.severity, "format": e.format, "args": args})
    });
    Json(events.collect())
}

pub struct EventDecoder;

impl Decoder for EventDecoder {
    fn matches(&self) -> PacketMatch {
        PacketMatch { packet_type: Some(PacketType::EVENT), meta: vec![] }
    }

    /// Packet: packet type, then per event: identifier, time tag, arguments in declaration order.
    fn decode(&self, packet: &[u8], _: &PacketMeta, dict: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        let error = |e: fgds_core::dictionary::TypeError| DecodeError(e.0);
        let bits = |name: &str, default| match dict.types.get(name) {
            Some(fgds_core::dictionary::Type::Int { bits, .. }) => *bits,
            _ => default,
        };
        let mut r = Reader::new(packet);
        r.uint(bits("FwPacketDescriptorType", 16)).map_err(error)?;
        // A packet may hold several events one after the other, as the Python GDS accepts.
        let mut records = Vec::new();
        while records.is_empty() || r.remaining() > 0 {
            let id = r.uint(bits("FwEventIdType", 32)).map_err(error)? as u32;
            let time = dict.decode_time(&mut r).map_err(error)?;
            let event = dict.event(id).ok_or_else(|| DecodeError(format!("event {id} is not in the dictionary")))?;
            let mut args = Map::new();
            for param in &event.params {
                let value = dict
                    .decode(&param.ty, &mut r)
                    .map_err(|e| DecodeError(format!("event {} argument {}: {}", event.name, param.name, e.0)))?;
                args.insert(param.name.clone(), value);
            }
            let values: Vec<_> = event.params.iter().map(|p| (&p.ty, &args[&p.name])).collect();
            let display_text = dict.format(&event.format, &values);
            records.push(NewRecord {
                id: Some(id),
                flight_time: Some(FlightTime {
                    base: time.base,
                    context: time.context,
                    seconds: time.seconds,
                    microseconds: time.microseconds,
                }),
                ..NewRecord::new(
                    KIND,
                    json!({"name": event.name, "severity": event.severity, "args": args, "display_text": display_text}),
                )
            });
        }
        Ok(records)
    }
}
