//! Text logs: the records of the run as lines of text, one log per record kind and one
//! (`gds`) for what the GDS itself reports (decode errors, unknown packets, discarded
//! bytes, plugin faults, storage gaps). Routes `/logs` and `/logs/:name`, and the Logs tab.

use fgds_core::axum::extract::{Path, Query as UrlQuery, State};
use fgds_core::axum::http::{header, StatusCode};
use fgds_core::axum::response::{IntoResponse, Response};
use fgds_core::axum::routing::get;
use fgds_core::axum::{Json, Router};
use fgds_core::serde_json::{json, Value};
use fgds_core::{
    Access, ApiError, Context, Manifest, Plugin, Query, Record, Registry, Result, RouteAccess, Routes, StaticFiles,
    UiBundle,
};
use std::collections::HashMap;

static FILES: StaticFiles = StaticFiles(&[("logs.js", include_bytes!("../ui/logs.js"))]);

/// The log of the GDS itself.
const GDS_LOG: &str = "gds";
const DEFAULT_LINES: u32 = 1000;
const MAX_LINES: u32 = 10000;

pub struct LogsPlugin;

impl Plugin for LogsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "logs", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_routes(Routes {
            prefix: "logs",
            router: Router::new().route("/", get(list)).route("/:name", get(lines)),
            access: vec![
                RouteAccess { method: "GET", path: "/", access: Access::Read },
                RouteAccess { method: "GET", path: "/:name", access: Access::Read },
            ],
        })?;
        reg.add_tab(UiBundle { tab_id: "logs", title: Some("Logs"), order: 80, module: "logs.js", files: &FILES })
    }
}

/// Whether a kind belongs to the log of the GDS itself.
fn is_gds_kind(kind: &str) -> bool {
    kind.starts_with("gds.") || kind.starts_with("raw.")
}

/// The kinds of a log, or `None` when there is no such log.
fn kinds_of(ctx: &Context, name: &str) -> Option<Vec<String>> {
    let kinds = ctx.records.kinds();
    if name == GDS_LOG {
        return Some(kinds.iter().filter(|k| is_gds_kind(&k.id)).map(|k| k.id.clone()).collect());
    }
    kinds.iter().find(|k| k.id == name && !is_gds_kind(&k.id)).map(|k| vec![k.id.clone()])
}

/// `GET /api/v1/logs`: the logs there are.
async fn list(State(ctx): State<Context>) -> Json<Value> {
    let kinds = ctx.records.kinds();
    let gds: Vec<&str> = kinds.iter().filter(|k| is_gds_kind(&k.id)).map(|k| k.id.as_str()).collect();
    let mut logs = vec![json!({"name": GDS_LOG, "description": "What the GDS reports about itself", "kinds": gds})];
    for kind in kinds.iter().filter(|k| !is_gds_kind(&k.id)) {
        logs.push(json!({"name": kind.id, "description": kind.description, "kinds": [kind.id]}));
    }
    Json(Value::Array(logs))
}

/// `GET /api/v1/logs/:name?after_seq=&limit=&format=`: lines of one log after `after_seq`,
/// oldest first. JSON `{"lines": [{"seq", "text"}], "next"}` by default; `format=text`
/// answers the lines alone as plain text.
async fn lines(
    State(ctx): State<Context>,
    Path(name): Path<String>,
    UrlQuery(options): UrlQuery<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let Some(kinds) = kinds_of(&ctx, &name) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found", format!("no log named {name}")));
    };
    let number = |key: &str| match options.get(key) {
        None => Ok(None),
        Some(text) => {
            text.parse::<u64>().map(Some).map_err(|_| ApiError::bad_request(format!("{key} is not a number")))
        }
    };
    if let Some(key) = options.keys().find(|k| !["after_seq", "limit", "format"].contains(&k.as_str())) {
        return Err(ApiError::bad_request(format!("unknown parameter {key}")));
    }
    let limit = number("limit")?.map_or(DEFAULT_LINES, |n| n.clamp(1, MAX_LINES as u64) as u32);
    let page = match kinds.is_empty() {
        true => Default::default(),
        false => {
            ctx.records.query(Query { kinds, after_seq: number("after_seq")?, limit, ..Default::default() }).await?
        }
    };
    match options.get("format").map(String::as_str) {
        None | Some("json") => {
            let lines: Vec<Value> =
                page.records.iter().map(|r| json!({"seq": r.seq.to_string(), "text": line(r)})).collect();
            Ok(Json(json!({"lines": lines, "next": page.next.map(|seq| seq.to_string())})).into_response())
        }
        Some("text") => {
            let text: String = page.records.iter().map(|r| line(r) + "\n").collect();
            Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response())
        }
        Some(other) => Err(ApiError::bad_request(format!("unknown format {other}"))),
    }
}

/// One record as one line: ground time, kind, then what the body holds. Line breaks in
/// the data are escaped so a record never spans lines.
pub fn line(record: &Record) -> String {
    let mut out = format!("{} {}", record.ground_time.to_iso(), record.kind);
    if let Some(t) = record.flight_time {
        out += &format!(" ({}:{}.{:06})", t.base, t.seconds, t.microseconds);
    }
    if let Some(id) = record.id {
        out += &format!(" 0x{id:x}");
    }
    let body = record.body.as_object();
    let text = |key: &str| body.and_then(|b| b.get(key)).and_then(Value::as_str);
    for key in ["name", "severity", "outcome"] {
        if let Some(value) = text(key) {
            out += &format!(" {value}");
        }
    }
    let rest = match (text("display_text"), body) {
        (Some(display), _) => display.to_string(),
        (None, Some(b)) => {
            let other: fgds_core::serde_json::Map<String, Value> = b
                .iter()
                .filter(|(k, _)| !["name", "severity", "outcome"].contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if other.is_empty() {
                String::new()
            } else {
                Value::Object(other).to_string()
            }
        }
        (None, None) => record.body.to_string(),
    };
    if !rest.is_empty() {
        out += ": ";
        out += &rest;
    }
    out.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgds_core::{FlightTime, Timestamp};

    #[test]
    fn a_record_is_one_line() {
        let record = Record {
            seq: 7,
            run: 1,
            ground_time: Timestamp(1_000_000),
            kind: "fprime.event".into(),
            flight_time: Some(FlightTime { base: 2, context: 0, seconds: 5, microseconds: 42 }),
            id: Some(0x10),
            entity: None,
            stored: true,
            raw: Default::default(),
            meta: Default::default(),
            body: json!({"name": "a.B", "severity": "ACTIVITY_HI", "args": {}, "display_text": "one\ntwo"}),
        };
        assert_eq!(
            line(&record),
            "1970-01-01T00:00:01.000000Z fprime.event (2:5.000042) 0x10 a.B ACTIVITY_HI: one\\ntwo"
        );
        let record =
            Record { body: json!({"name": "c.D", "outcome": "sent", "args": {"x": 1}}), flight_time: None, ..record };
        assert_eq!(line(&record), "1970-01-01T00:00:01.000000Z fprime.event 0x10 c.D sent: {\"args\":{\"x\":1}}");
    }
}
