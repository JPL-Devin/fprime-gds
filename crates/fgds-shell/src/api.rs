//! The core routes of `/api/v1` (section 4.7 of the design). Values are encoded as
//! API-4 requires by `Record::to_json`.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query as Params, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use fgds_core::{ApiError, Core, FieldFilter, Filter, Page, Query, StreamEnd, StreamItem, TimeRange, Timestamp};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;

type Shared = State<Arc<Core>>;
type Pairs = Params<Vec<(String, String)>>;
type Answer = Result<Json<Value>, ApiError>;

pub fn routes(core: Arc<Core>) -> Router {
    Router::new()
        .route("/version", get(version))
        .route("/status", get(status))
        .route("/dictionary", get(dictionary))
        .route("/runs", get(runs))
        .route("/kinds", get(kinds))
        .route("/records", get(records))
        .route("/records/latest", get(latest))
        .route("/cursor", get(cursor))
        .route("/stream", get(stream))
        .route("/raw", post(raw))
        .route("/link", post(link))
        .with_state(core)
}

async fn version(State(core): Shared) -> Json<Value> {
    let plugins = core.registry.manifests.iter();
    Json(json!({
        "api": crate::API_VERSION,
        "gds": fgds_core::VERSION,
        "dictionary": core.dictionary.version(),
        "plugins": plugins.map(|m| json!({"name": m.name, "version": m.version})).collect::<Vec<_>>(),
    }))
}

async fn status(State(core): Shared) -> Json<Value> {
    let mut status = core.status.snapshot();
    status["run"] = json!(core.run.run);
    status["cursor"] = json!(core.records.cursor().to_string());
    Json(status)
}

async fn dictionary(State(core): Shared, Params(params): Pairs) -> Answer {
    // Only the current run's dictionary exists until a storage keeps earlier runs.
    match params.iter().find(|(key, _)| key == "run") {
        Some((_, run)) if *run != core.run.run.to_string() => {
            Err(ApiError::new(StatusCode::NOT_FOUND, "not_found", format!("no dictionary for run {run}")))
        }
        _ => Ok(Json(core.dictionary.json.clone())),
    }
}

async fn runs(State(core): Shared) -> Answer {
    let runs = core.records.runs().await?;
    Ok(Json(runs.iter().map(|r| r.to_json()).collect()))
}

async fn kinds(State(core): Shared) -> Json<Value> {
    Json(core.records.kinds().iter().map(|k| k.to_json()).collect())
}

async fn cursor(State(core): Shared) -> Json<Value> {
    Json(json!({"seq": core.records.cursor().to_string(), "run": core.run.run}))
}

fn page(page: Page) -> Json<Value> {
    let records: Vec<Value> = page.records.iter().map(|r| r.to_json()).collect();
    Json(json!({"records": records, "next": page.next.map(|seq| seq.to_string())}))
}

async fn records(State(core): Shared, Params(params): Pairs) -> Answer {
    Ok(page(core.records.query(query(&params)?).await?))
}

async fn latest(State(core): Shared, Params(params): Pairs) -> Answer {
    Ok(page(core.records.latest(query(&params)?).await?))
}

/// The query of a request. Parameters: `kind` and `id` (repeatable or comma-separated),
/// `run`, `after_seq`, `before_seq`, `from` and `to` (ground time, microseconds since
/// 1970), `text`, `limit`, and `filter=<field>:<op>:<value>` (repeatable) on declared
/// fields. Anything else, or a value that does not parse, is a 400.
fn query(params: &[(String, String)]) -> Result<Query, ApiError> {
    fn number<T: std::str::FromStr>(key: &str, value: &str) -> Result<T, ApiError> {
        value.parse().map_err(|_| ApiError::bad_request(format!("{key}={value} is not a valid number")))
    }
    let mut q = Query::default();
    let mut time = TimeRange::default();
    for (key, value) in params {
        match key.as_str() {
            "kind" => q.kinds.extend(value.split(',').filter(|k| !k.is_empty()).map(str::to_string)),
            "id" => {
                for id in value.split(',') {
                    q.ids.push(number(key, id)?);
                }
            }
            "run" => q.run = Some(number(key, value)?),
            "after_seq" => q.after_seq = Some(number(key, value)?),
            "before_seq" => q.before_seq = Some(number(key, value)?),
            "from" => time.from = Some(Timestamp(number(key, value)?)),
            "to" => time.to = Some(Timestamp(number(key, value)?)),
            "text" => q.text = Some(value.clone()),
            "limit" => q.limit = number(key, value)?,
            "filter" => {
                let mut parts = value.splitn(3, ':');
                let (Some(field), Some(op), Some(operand)) = (parts.next(), parts.next(), parts.next()) else {
                    return Err(ApiError::bad_request(format!("filter={value} is not <field>:<op>:<value>")));
                };
                q.filters.push(FieldFilter { field: field.into(), op: op.parse()?, value: json!(operand) });
            }
            _ => return Err(ApiError::bad_request(format!("unknown parameter \"{key}\""))),
        }
    }
    if time.from.is_some() || time.to.is_some() {
        q.time = Some(time);
    }
    Ok(q)
}

/// `WS /api/v1/stream?kind=&after_seq=`: stored records after `after_seq`, then live
/// ones, each as `{"type": "record", "record": ...}`. Without `after_seq`, live only.
/// `{"type": "live", "seq": ...}` is sent once, when the stored records have all been
/// sent and every later record is live; `seq` is the last record sent (0 for none).
/// A client that falls too far behind gets `{"type": "lagged", "last_seq": ...}` and
/// the connection is closed; it reconnects with `after_seq=<last_seq>` (API-6).
async fn stream(
    State(core): Shared,
    Params(params): Pairs,
    headers: axum::http::HeaderMap,
    upgrade: Option<WebSocketUpgrade>,
) -> Result<Response, ApiError> {
    // A page of another site must not read the stream through the operator's browser:
    // a browser always sends `Origin`, and it must be this server.
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(origin) = text("origin") {
        let host = origin.split_once("://").map_or(origin, |(_, host)| host);
        if Some(host) != text("host") {
            let message = format!("origin {origin} is not this server");
            return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", message));
        }
    }
    let q = query(&params)?;
    if let Some((key, _)) = params.iter().find(|(key, _)| key != "kind" && key != "after_seq") {
        return Err(ApiError::bad_request(format!("the stream does not take \"{key}\"")));
    }
    // Checks the kinds exist before the upgrade, so a mistake is a 400 answer.
    core.records.query(Query { kinds: q.kinds.clone(), limit: 1, ..Default::default() }).await?;
    let upgrade = upgrade.ok_or_else(|| ApiError::bad_request("the stream is a WebSocket"))?;
    Ok(upgrade.on_upgrade(move |socket| feed(socket, core, q)))
}

async fn feed(mut socket: WebSocket, core: Arc<Core>, q: Query) {
    let mut records = core.records.subscribe(Filter { kinds: q.kinds }, q.after_seq);
    loop {
        let message = tokio::select! {
            next = records.next_item() => match next {
                Ok(StreamItem::Record(record)) => json!({"type": "record", "record": record.to_json()}),
                Ok(StreamItem::Live { seq }) => json!({"type": "live", "seq": seq.to_string()}),
                Err(StreamEnd::Lagged { last_seq }) => {
                    let lagged = json!({"type": "lagged", "last_seq": last_seq.to_string()});
                    let limit = std::time::Duration::from_secs(1);
                    let _ = tokio::time::timeout(limit, socket.send(Message::Text(lagged.to_string()))).await;
                    return;
                }
                Err(StreamEnd::Closed) => return,
            },
            // The client closed, or sent something: this stream takes no input.
            received = socket.recv() => match received {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => continue,
            },
        };
        // A client that stopped reading must not hold the task and its records for ever.
        let limit = std::time::Duration::from_millis(crate::STREAM_SEND_TIMEOUT_MS.load(Ordering::Relaxed));
        match tokio::time::timeout(limit, socket.send(Message::Text(message.to_string()))).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return,
            Err(_) => {
                tracing::warn!("stream client does not read: connection dropped");
                return;
            }
        }
    }
}

/// `POST /api/v1/raw {"data": <base64>}`: send bytes as one packet, framed like any other.
async fn raw(State(core): Shared, headers: axum::http::HeaderMap, Json(body): Json<Value>) -> Answer {
    let client = headers.get("x-fgds-client").and_then(|v| v.to_str().ok()).unwrap_or("api");
    let data = body["data"].as_str().ok_or_else(|| ApiError::bad_request("\"data\" is missing"))?;
    let packet = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| ApiError::bad_request(format!("\"data\" is not base64: {e}")))?;
    let seq = core.uplink.send_packet(&packet, &Default::default(), client, true).await?;
    Ok(Json(json!({"sent": packet.len(), "seq": seq.to_string()})))
}

/// `POST /api/v1/link {"enabled": bool}` (LNK-22).
async fn link(State(core): Shared, Json(body): Json<Value>) -> Answer {
    let enabled = body["enabled"].as_bool().ok_or_else(|| ApiError::bad_request("\"enabled\" is not a boolean"))?;
    core.link.set_enabled(enabled);
    tracing::info!(enabled, "link switched through the API");
    Ok(Json(json!({"enabled": enabled})))
}
