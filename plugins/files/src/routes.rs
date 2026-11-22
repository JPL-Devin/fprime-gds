//! Routes under `/api/v1/files`: upload and queue control, downlink list and download.

use crate::{base_name, path_inside, publish, Shared, State, Transfer, KIND_UPLINK};
use fgds_core::axum::body::Body;
use fgds_core::axum::extract::{Path, Query, State as Ctx};
use fgds_core::axum::http::{header, StatusCode};
use fgds_core::axum::response::{IntoResponse, Response};
use fgds_core::axum::routing::{get, post};
use fgds_core::axum::{Extension, Json, Router};
use fgds_core::serde_json::{json, Value};
use fgds_core::{Access, ApiError, Context, RouteAccess, Routes};
use serde::Deserialize;
use std::sync::Arc;

type Files = Extension<Arc<Shared>>;

pub(crate) fn routes(shared: Arc<Shared>) -> Routes {
    let router = Router::new()
        .route("/uplink", get(queue).post(upload))
        .route("/uplink/pause", post(pause))
        .route("/uplink/resume", post(resume))
        .route("/uplink/cancel-packet", post(cancel_packet))
        .route("/uplink/:id/cancel", post(cancel))
        .route("/downlink", get(downlinks))
        .route("/downlink/:name", get(download))
        .layer(Extension(shared));
    let access = |method, path, access| RouteAccess { method, path, access };
    Routes {
        prefix: "files",
        router,
        access: vec![
            access("GET", "/uplink", Access::Read),
            access("POST", "/uplink", Access::Write),
            access("POST", "/uplink/pause", Access::Write),
            access("POST", "/uplink/resume", Access::Write),
            access("POST", "/uplink/cancel-packet", Access::Write),
            access("POST", "/uplink/:id/cancel", Access::Write),
            access("GET", "/downlink", Access::Read),
            access("GET", "/downlink/:name", Access::Read),
        ],
    }
}

fn error(status: StatusCode, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(status, code, message)
}

fn queue_json(ctx: &Context, shared: &Shared, state: &State) -> Value {
    json!({
        "paused": state.paused,
        "link": ctx.status.link(),
        // Where uploads are staged: the source path of each transfer starts with it.
        "directory": shared.options(&ctx.config).ok().map(|options| options.uplink_dir()),
        "files": state.uplinks.iter().map(Transfer::to_json).collect::<Vec<_>>(),
    })
}

async fn queue(Ctx(ctx): Ctx<Context>, Extension(shared): Files) -> Json<Value> {
    Json(queue_json(&ctx, &shared, &shared.state()))
}

#[derive(Deserialize)]
struct Upload {
    /// Name of the file; reduced to its base name (FIL-3).
    name: String,
    /// Path on the spacecraft, passed unchanged.
    destination: Option<String>,
    /// Without a destination: the directory on the spacecraft the base name is joined to, `/` when absent
    /// (`uplink_destination`).
    directory: Option<String>,
    /// Source path written in the start packet; the path of the staged copy when absent. A script
    /// gives the path of its own file, as the uplinker of the Python GDS sends it.
    source: Option<String>,
    /// Data packets to send, numbered from 1 and separated by commas; every packet when absent or empty.
    packets: Option<String>,
}

/// The request body is the content of the file.
async fn upload(
    Ctx(ctx): Ctx<Context>,
    Extension(shared): Files,
    Query(upload): Query<Upload>,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    let options = shared.options(&ctx.config)?;
    let invalid = |message: String| error(StatusCode::UNPROCESSABLE_ENTITY, "invalid", message);
    let name = base_name(&upload.name).map_err(invalid)?.to_string();
    let directory = options.uplink_dir();
    let path = path_inside(&directory, &name).map_err(invalid)?;
    let destination = crate::uplink_destination(upload.destination.as_deref(), upload.directory.as_deref(), &name);
    if destination.len() > 255 {
        return Err(invalid("the destination path is longer than 255 bytes".into()));
    }
    let too_large = || {
        let message = format!("the file is larger than {} bytes", options.max_upload);
        error(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message)
    };
    let content = fgds_core::axum::body::to_bytes(body, options.max_upload).await.map_err(|_| too_large())?;
    if u32::try_from(content.len()).is_err() {
        return Err(too_large());
    }
    // The path of the staged copy is the source path of the start packet, as with the
    // Python GDS; a path too long for the packet is replaced by the name.
    let source = upload.source.filter(|s| !s.is_empty()).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let source = if source.len() > 255 { name.clone() } else { source };
    // As `TransmitFile` of the Python GDS: sorted, each number once; an empty list is the whole file.
    let mut packets = Vec::new();
    for token in upload.packets.iter().flat_map(|list| list.split(',')).map(str::trim).filter(|t| !t.is_empty()) {
        match token.parse::<u64>() {
            Ok(number) if number > 0 => packets.push(number),
            _ => return Err(invalid("Specify packet numbers as comma-separated list".into())),
        }
    }
    packets.sort_unstable();
    packets.dedup();
    let mut transfer = Transfer {
        id: shared.new_id("up"),
        name,
        source,
        destination,
        size: content.len() as u64,
        done: 0,
        state: "queued",
        reason: None,
        problems: 0,
        cancel: false,
        started: None,
        ended: None,
        packets: (!packets.is_empty()).then_some(packets),
    };
    {
        // Reserved under the lock: two uploads of one name cannot share the staged copy.
        let mut state = shared.state();
        if state.uplinks.iter().any(|t| t.name == transfer.name && !t.ended()) {
            let message = format!("{} is already in the uplink queue", transfer.name);
            return Err(error(StatusCode::CONFLICT, "conflict", message));
        }
        state.uplinks.push(transfer.clone());
        State::trim(&mut state.uplinks);
    }
    if let Err(e) = std::fs::create_dir_all(&directory).and_then(|_| std::fs::write(&path, &content)) {
        let mut state = shared.state();
        if let Some(listed) = state.uplinks.iter_mut().find(|t| t.id == transfer.id) {
            (listed.state, listed.reason) = ("failed", Some(e.to_string()));
            transfer = listed.clone();
        }
    }
    publish(&ctx, transfer.record(KIND_UPLINK)).await;
    shared.wake.notify_one();
    match transfer.reason {
        Some(reason) => Err(error(StatusCode::INTERNAL_SERVER_ERROR, "internal", reason)),
        None => Ok(Json(transfer.to_json())),
    }
}

/// Pause the queue. The file being sent is canceled, as with the Python GDS.
async fn pause(Ctx(ctx): Ctx<Context>, Extension(shared): Files) -> Json<Value> {
    let mut state = shared.state();
    state.paused = true;
    Json(queue_json(&ctx, &shared, &state))
}

/// Resume the queue; refused while the link is not connected (FIL-4).
async fn resume(Ctx(ctx): Ctx<Context>, Extension(shared): Files) -> Result<Json<Value>, ApiError> {
    if ctx.status.link() != "connected" {
        let message = "the link is not connected: the uplink queue stays paused";
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, "link_down", message));
    }
    let mut state = shared.state();
    state.paused = false;
    shared.wake.notify_one();
    Ok(Json(queue_json(&ctx, &shared, &state)))
}

/// Send a cancel packet by hand. Its packet index is 0, as `send_cancel_packet` of the Python uplinker gives when
/// no file is being sent (the index starts again at zero at the end of each file). While a file is being sent
/// it is refused: the flight side would drop the rest of the file, and the Python page disables its button then.
async fn cancel_packet(Ctx(ctx): Ctx<Context>, Extension(shared): Files) -> Result<Json<Value>, ApiError> {
    if shared.state().uplinks.iter().any(|t| t.state == "transferring") {
        let message = "a file is being sent: cancel that uplink instead";
        return Err(error(StatusCode::CONFLICT, "conflict", message));
    }
    let bytes =
        crate::packet::encode(crate::packet::descriptor_bytes(&ctx.dictionary), 0, &crate::packet::FilePacket::Cancel);
    match ctx.uplink.send_packet(&bytes, &Default::default(), "files", false).await {
        Ok(_) => Ok(Json(json!({ "sequence": 0 }))),
        Err(e) => Err(error(StatusCode::SERVICE_UNAVAILABLE, "link_down", e.to_string())),
    }
}

/// Cancel the file being sent, or remove a queued file.
async fn cancel(
    Ctx(ctx): Ctx<Context>,
    Extension(shared): Files,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let removed = {
        let mut state = shared.state();
        let Some(transfer) = state.uplinks.iter_mut().find(|t| t.id == id) else {
            return Err(error(StatusCode::NOT_FOUND, "not_found", "no uplink has this identifier"));
        };
        match transfer.state {
            "queued" => {
                transfer.state = "removed";
                Some(transfer.clone())
            }
            "transferring" => {
                transfer.cancel = true;
                None
            }
            state => {
                let message = format!("the uplink has ended ({state})");
                return Err(error(StatusCode::CONFLICT, "conflict", message));
            }
        }
    };
    if let Some(transfer) = &removed {
        if let Ok(options) = shared.options(&ctx.config) {
            let _ = std::fs::remove_file(options.uplink_dir().join(&transfer.name));
        }
        publish(&ctx, transfer.record(KIND_UPLINK)).await;
    }
    Ok(Json(queue_json(&ctx, &shared, &shared.state())))
}

/// The downlinks of this run, and the files of the downlink directory.
async fn downlinks(Ctx(ctx): Ctx<Context>, Extension(shared): Files) -> Result<Json<Value>, ApiError> {
    let directory = shared.options(&ctx.config)?.downlink_dir();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&directory).into_iter().flatten().flatten() {
        if let (Ok(meta), Some(name)) = (entry.metadata(), entry.file_name().to_str()) {
            if meta.is_file() {
                files.push((name.to_string(), meta.len()));
            }
        }
    }
    files.sort();
    let files: Vec<_> = files.iter().map(|(name, size)| json!({"name": name, "size": size})).collect();
    let transfers: Vec<_> = shared.state().downlinks.iter().map(Transfer::to_json).collect();
    Ok(Json(json!({"transfers": transfers, "files": files})))
}

/// A downlinked file, as an attachment the browser does not interpret (UI-6).
async fn download(
    Ctx(ctx): Ctx<Context>,
    Extension(shared): Files,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let not_found = || error(StatusCode::NOT_FOUND, "not_found", "no downlinked file has this name");
    let directory = shared.options(&ctx.config)?.downlink_dir();
    // The name must be a base name already: nothing is stripped from a request (FIL-3).
    if base_name(&name) != Ok(name.as_str()) {
        return Err(not_found());
    }
    let path = path_inside(&directory, &name).map_err(|_| not_found())?;
    // A link out of the directory is not followed.
    let inside = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file());
    let content = if inside { std::fs::read(&path).map_err(|_| not_found())? } else { return Err(not_found()) };
    // Only characters that need no quoting; the full name is in the UTF-8 form.
    let plain: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let headers = [
        (header::CONTENT_TYPE, "application/octet-stream".to_string()),
        (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}")),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
    ];
    Ok((headers, content).into_response())
}
