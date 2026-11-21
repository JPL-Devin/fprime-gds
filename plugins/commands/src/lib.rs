//! F Prime commands: the encoder of request kind `command` (records of kind `command`)
//! and the routes that send a command and list the command definitions.

use base64::Engine;
use fgds_core::axum::extract::{Query, State};
use fgds_core::axum::http::HeaderMap;
use fgds_core::axum::routing::{get, post};
use fgds_core::axum::{Json, Router};
use fgds_core::dictionary::Type;
use fgds_core::serde_json::{json, Value};
use fgds_core::{
    Access, ApiError, Context, Dictionary, Encoded, Encoder, FieldSpec, FieldType, KindSpec, Manifest, PacketType,
    Plugin, Registry, Result, RouteAccess, Routes, ValidationError,
};
use fgds_core::{StaticFiles, UiBundle};
use std::sync::Arc;

pub const KIND: &str = "fprime.command";
/// The request kind of the uplink service (section 4.5).
pub const REQUEST: &str = "command";

/// The tab module, embedded in the binary.
static UI_FILES: StaticFiles = StaticFiles(&[("commands.js", include_bytes!("../ui/commands.js"))]);

pub struct CommandsPlugin;

impl Plugin for CommandsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "commands", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec {
            fields: &[
                FieldSpec { name: "name", pointer: "/name", ty: FieldType::Text },
                FieldSpec { name: "outcome", pointer: "/outcome", ty: FieldType::Text },
            ],
            text_fields: &["/name"],
            has_entities: true,
            ..KindSpec::new(KIND, "A command send, accepted or not; id is the opcode")
        })?;
        reg.add_encoder(REQUEST, Arc::new(|_| Ok(Box::new(CommandEncoder))))?;
        reg.add_tab(UiBundle {
            tab_id: "commands",
            title: Some("Commanding"),
            order: 10,
            module: "commands.js",
            files: &UI_FILES,
        })?;
        reg.add_routes(Routes {
            prefix: "commands",
            router: Router::new().route("/", post(send)).route("/definitions", get(definitions)),
            access: vec![
                RouteAccess { method: "POST", path: "/", access: Access::Write },
                RouteAccess { method: "GET", path: "/definitions", access: Access::Read },
            ],
        })
    }
}

#[derive(serde::Deserialize)]
struct SendOptions {
    /// Validate and encode only (LNK-21).
    #[serde(default)]
    dry_run: bool,
}

/// `POST /api/v1/commands`: body `{"name": ..., "args": [...]}`; `args` may also be an
/// object of argument names. Answers 200 with the record's `seq` and `entity`.
async fn send(
    State(ctx): State<Context>,
    Query(options): Query<SendOptions>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if options.dry_run {
        let encoded = ctx.uplink.dry_run(REQUEST, &request)?;
        let packet = base64::engine::general_purpose::STANDARD.encode(&encoded.packet);
        return Ok(Json(json!({"dry_run": true, "packet": packet, "command": encoded.body})));
    }
    let client = headers.get("x-fgds-client").and_then(|v| v.to_str().ok()).unwrap_or("api");
    let sent = ctx.uplink.send(REQUEST, &request, client).await?;
    Ok(Json(json!({"seq": sent.seq.to_string(), "entity": sent.entity})))
}

/// The commands the dictionary defines.
async fn definitions(State(ctx): State<Context>) -> Json<Value> {
    let commands = ctx.dictionary.commands.iter().map(|c| {
        let args: Vec<&str> = c.params.iter().map(|p| p.name.as_str()).collect();
        json!({"name": c.name, "opcode": c.opcode, "args": args})
    });
    Json(commands.collect())
}

pub struct CommandEncoder;

impl Encoder for CommandEncoder {
    fn request_kind(&self) -> &'static str {
        REQUEST
    }

    fn record_kind(&self) -> &'static str {
        KIND
    }

    /// Packet: packet type, opcode, arguments in declaration order.
    fn encode(&self, request: &Value, dict: &Dictionary) -> Result<Encoded, ValidationError> {
        let name = request["name"].as_str().ok_or_else(|| ValidationError("\"name\" is missing".into()))?;
        let command = dict.encode_command(name, &request["args"]).map_err(|e| ValidationError(e.0))?;
        let mut packet = Vec::new();
        let descriptor = dict.config_type("FwPacketDescriptorType", Type::U16);
        dict.encode(&descriptor, &json!(PacketType::COMMAND.0), &mut packet)
            .map_err(|e| ValidationError(format!("packet type: {}", e.0)))?;
        packet.extend_from_slice(&command.bytes);
        Ok(Encoded {
            packet: packet.into(),
            id: Some(command.opcode),
            body: json!({"name": name, "opcode": command.opcode, "args": command.args}),
            meta: Default::default(),
        })
    }
}
