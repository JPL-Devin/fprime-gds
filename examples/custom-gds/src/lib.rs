//! What a project adds to the stock GDS, one plugin per feature:
//!
//! - `StationFramingPlugin`: a framing layer around the F Prime frame.
//! - `CameraPlugin`: a decoder registered by application identifier (CCSDS framing).
//! - `NotesPlugin`: a record kind, its route, a browser tab and a subcommand.
//! - `FaultyPlugin`: a handler that panics, to show that the GDS contains it.
//!
//! `main.rs` composes them with the default plugins; `bin/conflict.rs` shows the
//! startup failure when two plugins register the same thing.

use fgds::core::axum::extract::State;
use fgds::core::axum::routing::get;
use fgds::core::axum::Json;
use fgds::core::bytes::{Buf, BufMut, BytesMut};
use fgds::core::serde_json::{json, Value};
use fgds::core::*;
use std::process::ExitCode;
use std::sync::Arc;

/// Every plugin of this project's GDS; the composition used by `main.rs` and the tests.
pub fn gds() -> fgds::GdsBuilder {
    fgds::Gds::builder()
        .with_defaults()
        .without("channels")
        .with(StationFramingPlugin)
        .with(framing_ccsds::CcsdsFramingPlugin)
        .with(CameraPlugin)
        .with(NotesPlugin)
        .with(FaultyPlugin)
}

// A framing layer: the ground station wraps each chunk of the link in
// `"GS"`, a station number and a 16-bit length.

const MARKER: [u8; 2] = *b"GS";
const HEADER: usize = 5;

pub struct StationFramingPlugin;

impl Plugin for StationFramingPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "framing-station",
            version: env!("CARGO_PKG_VERSION"),
            options: vec![OptionSpec {
                name: "station",
                help: "Station number written in frames sent to the flight software",
                default: Some("1"),
                secret: false,
            }],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_framing_layer("station", Arc::new(StationFraming))?;
        // Outermost first: station frames carry F Prime frames.
        reg.add_framing_profile("station-fprime", &["station", "fprime"])
    }
}

struct StationFraming;

impl FramingLayer for StationFraming {
    fn deframer(&self, _: &PluginConfig, _: &PeerInfo) -> Box<dyn Deframe> {
        Box::new(StationFraming)
    }
    fn framer(&self, cfg: &PluginConfig) -> Box<dyn Frame> {
        Box::new(StationFramer { station: cfg.get("station").and_then(|s| s.parse().ok()).unwrap_or(1) })
    }
}

impl Deframe for StationFraming {
    fn deframe(&mut self, input: &mut BytesMut, out: &mut Vec<Deframed>) -> Result<()> {
        loop {
            // Drop whatever precedes the next marker.
            let start = input.windows(2).position(|w| w == MARKER).unwrap_or(input.len().saturating_sub(1));
            if start > 0 {
                out.push(Deframed::Discarded {
                    bytes: input.split_to(start).freeze(),
                    reason: "no station marker".into(),
                });
            }
            if input.len() < HEADER {
                return Ok(());
            }
            let length = u16::from_be_bytes([input[3], input[4]]) as usize;
            if input.len() < HEADER + length {
                return Ok(());
            }
            // The station number becomes packet metadata, which decoders can match on.
            let meta = PacketMeta::default().with("station", MetaValue::U64(input[2].into()));
            input.advance(HEADER);
            out.push(Deframed::Packet { bytes: input.split_to(length).freeze(), meta });
        }
    }
}

struct StationFramer {
    station: u8,
}

impl Frame for StationFramer {
    fn frame(&mut self, packet: &[u8], _: &PacketMeta, out: &mut BytesMut) -> Result<()> {
        let length = u16::try_from(packet.len()).map_err(|_| Error::other("frame too long for a station frame"))?;
        out.put_slice(&MARKER);
        out.put_u8(self.station);
        out.put_u16(length);
        out.put_slice(packet);
        Ok(())
    }
}

// A decoder registered by application identifier: the camera sends its own Space Packets,
// which are not F Prime packets. Needs a CCSDS framing profile (`--framing ccsds`).

pub const CAMERA: &str = "example.camera";
pub const CAMERA_APID: u64 = 100;

pub struct CameraPlugin;

impl Plugin for CameraPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "camera", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec::new(CAMERA, "A camera status packet; id is the exposure number"))?;
        reg.add_decoder(Arc::new(CameraDecoder))
    }
}

struct CameraDecoder;

impl Decoder for CameraDecoder {
    /// Any packet of the camera's application identifier, whatever its first bytes are.
    fn matches(&self) -> PacketMatch {
        PacketMatch { packet_type: None, meta: vec![(framing_ccsds::META_APID, MetaValue::U64(CAMERA_APID))] }
    }

    /// Packet: exposure number (32 bits), temperature in tenths of a degree (16 bits, signed).
    fn decode(&self, packet: &[u8], _: &PacketMeta, _: &Dictionary) -> std::result::Result<Vec<NewRecord>, DecodeError> {
        let [a, b, c, d, high, low] = *packet else {
            return Err(DecodeError(format!("a camera packet has 6 bytes, not {}", packet.len())));
        };
        let exposure = u32::from_be_bytes([a, b, c, d]);
        let temperature = f64::from(i16::from_be_bytes([high, low])) / 10.0;
        Ok(vec![NewRecord { id: Some(exposure), ..NewRecord::new(CAMERA, json!({"temperature": temperature})) }])
    }
}

// A record kind with its route, tab and subcommand: notes written by operators.

pub const NOTE: &str = "example.note";

static NOTES_FILES: StaticFiles = StaticFiles(&[("notes.js", include_bytes!("../ui/notes.js"))]);

pub struct NotesPlugin;

impl Plugin for NotesPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "notes", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec {
            fields: &[FieldSpec { name: "author", pointer: "/author", ty: FieldType::Text }],
            text_fields: &["/text"],
            ..KindSpec::new(NOTE, "A note written by an operator")
        })?;
        let router = axum::Router::new().route("/", get(list_notes).post(add_note));
        reg.add_routes(Routes {
            prefix: "notes",
            router,
            access: vec![
                RouteAccess { method: "GET", path: "/", access: Access::Read },
                RouteAccess { method: "POST", path: "/", access: Access::Write },
            ],
        })?;
        reg.add_tab(UiBundle {
            tab_id: "notes",
            title: Some("Notes"),
            order: 100,
            module: "notes.js",
            files: &NOTES_FILES,
        })?;
        reg.add_subcommand(Arc::new(KindsCommand))
    }
}

/// `POST /api/v1/notes {"text": ..., "author": ...}`: store a note.
async fn add_note(State(ctx): State<Context>, Json(body): Json<Value>) -> std::result::Result<Json<Value>, ApiError> {
    let text = body["text"].as_str().ok_or_else(|| ApiError::bad_request("\"text\" is missing"))?;
    let author = body["author"].as_str().unwrap_or("unknown");
    let seq = ctx.records.publish(NewRecord::new(NOTE, json!({"text": text, "author": author}))).await?;
    Ok(Json(json!({"seq": seq.to_string()})))
}

/// `GET /api/v1/notes`: the text of every note of this run, oldest first.
async fn list_notes(State(ctx): State<Context>) -> std::result::Result<Json<Value>, ApiError> {
    let page = ctx.records.query(Query { kinds: vec![NOTE.into()], ..Default::default() }).await?;
    Ok(Json(page.records.iter().map(|r| r.body["text"].clone()).collect()))
}

/// `custom-gds kinds`: an offline subcommand listing the record kinds of this binary.
struct KindsCommand;

impl Subcommand for KindsCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("kinds").about("List the record kinds this GDS stores")
    }

    fn run(&self, _: &clap::ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        for kind in &env.registry.kinds {
            println!("{}: {}", kind.id, kind.description);
        }
        Ok(ExitCode::SUCCESS)
    }
}

// A plugin with a bug: its handler panics on a note that says "panic".

pub struct FaultyPlugin;

impl Plugin for FaultyPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            name: "faulty",
            version: env!("CARGO_PKG_VERSION"),
            requires_kinds: vec![NOTE],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_handler(Arc::new(|_| Ok(Box::new(FaultyHandler))))?;
        Ok(())
    }
}

struct FaultyHandler;

#[async_trait]
impl Handler for FaultyHandler {
    fn subscription(&self) -> Filter {
        Filter { kinds: vec![NOTE.into()] }
    }

    async fn run(self: Box<Self>, _: Context, mut records: RecordStream) -> Result<()> {
        while let Some(record) = records.next().await {
            assert!(record.body["text"] != "panic", "a note said panic");
        }
        Ok(())
    }
}

/// Registers the kind `NotesPlugin` owns: the two cannot be in one binary.
pub struct ConflictingPlugin;

impl Plugin for ConflictingPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "other-notes", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec::new(NOTE, "Another idea of a note"))
    }
}
