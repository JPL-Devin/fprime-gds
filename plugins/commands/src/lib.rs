//! F Prime commands: the encoder of request kind `command` (records of kind `command`)
//! and the routes that send a command and list the command definitions.

use base64::Engine;
use fgds_core::axum::extract::{Query, State};
use fgds_core::axum::http::HeaderMap;
use fgds_core::axum::routing::{get, post};
use fgds_core::axum::{Json, Router};
use fgds_core::dictionary::Type;
use fgds_core::serde_json::{json, Map, Value};
use fgds_core::{
    Access, ApiError, Context, Dictionary, Encoded, Encoder, FieldSpec, FieldType, KindSpec,
    Manifest, PacketType, Plugin, Registry, Result, RouteAccess, Routes, ValidationError,
};
use std::sync::Arc;

pub const KIND: &str = "command";

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
        reg.add_encoder(Arc::new(CommandEncoder))?;
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
        let encoded = ctx.uplink.dry_run(KIND, &request)?;
        let packet = base64::engine::general_purpose::STANDARD.encode(&encoded.packet);
        return Ok(Json(json!({"dry_run": true, "packet": packet, "command": encoded.body})));
    }
    let client = headers.get("x-fgds-client").and_then(|v| v.to_str().ok()).unwrap_or("api");
    let sent = ctx.uplink.send(KIND, &request, client).await?;
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
        KIND
    }

    fn record_kind(&self) -> &'static str {
        KIND
    }

    /// Packet: packet type, opcode, arguments in declaration order.
    fn encode(&self, request: &Value, dict: &Dictionary) -> Result<Encoded, ValidationError> {
        let invalid = |message: String| ValidationError(message);
        let name = request["name"].as_str().ok_or_else(|| invalid("\"name\" is missing".into()))?;
        let command = dict.command(name).ok_or_else(|| invalid(format!("unknown command {name}")))?;
        let params = &command.params;
        let count = |n: usize| match n == params.len() {
            true => Ok(()),
            false => Err(invalid(format!("{name} takes {} arguments, {n} given", params.len()))),
        };
        // Arguments in declaration order, whichever way they were given.
        let values: Vec<&Value> = match &request["args"] {
            Value::Null => count(0).map(|_| Vec::new())?,
            Value::Array(values) => count(values.len()).map(|_| values.iter().collect())?,
            Value::Object(by_name) => {
                if let Some(extra) = by_name.keys().find(|k| !params.iter().any(|p| p.name == **k)) {
                    return Err(invalid(format!("{name} has no argument {extra}")));
                }
                let find = |p: &fgds_core::dictionary::Param| {
                    by_name.get(&p.name).ok_or_else(|| invalid(format!("argument {} is missing", p.name)))
                };
                params.iter().map(find).collect::<Result<_, _>>()?
            }
            _ => return Err(invalid("\"args\" is neither a list nor an object".into())),
        };

        let width = |name: &str, default: Type| dict.config_type(name, default);
        let mut packet = Vec::new();
        let put = |packet: &mut Vec<u8>, ty: &Type, value: &Value, what: &str| {
            dict.encode(ty, value, packet).map_err(|e| invalid(format!("{what}: {}", e.0)))
        };
        let packet_type = json!(PacketType::COMMAND.0);
        put(&mut packet, &width("FwPacketDescriptorType", Type::U16), &packet_type, "packet type")?;
        put(&mut packet, &width("FwOpcodeType", Type::U32), &json!(command.opcode), "opcode")?;
        let mut args = Map::new();
        for (param, value) in params.iter().zip(values) {
            let start = packet.len();
            put(&mut packet, &param.ty, value, &format!("argument {}", param.name))?;
            // Record what was sent, not what was asked: decode the bytes just written.
            let sent = dict.decode(&param.ty, &mut fgds_core::dictionary::Reader::new(&packet[start..]));
            args.insert(param.name.clone(), sent.unwrap_or_else(|_| value.clone()));
        }
        Ok(Encoded {
            packet: packet.into(),
            id: Some(command.opcode),
            body: json!({"name": command.name, "opcode": command.opcode, "args": args}),
        })
    }
}
