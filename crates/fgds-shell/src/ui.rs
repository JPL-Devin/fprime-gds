//! The page shell and the tab modules embedded in the plugins.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use fgds_core::Core;
use serde_json::{json, Value};
use std::sync::Arc;

pub fn routes(core: Arc<Core>) -> Router {
    Router::new()
        .route("/", get(|| async { file("index.html", include_bytes!("../ui/index.html")) }))
        .route("/ui/shell.js", get(|| async { file("shell.js", include_bytes!("../ui/shell.js")) }))
        .route("/ui/shell.css", get(|| async { file("shell.css", include_bytes!("../ui/shell.css")) }))
        .route("/ui/tabs", get(tabs))
        .route("/ui/plugins/:tab/*path", get(plugin_file))
        .with_state(core)
}

fn file(name: &str, content: &'static [u8]) -> Response {
    let content_type = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, content_type)], content).into_response()
}

/// The tabs in display order, each with the address of its module.
async fn tabs(State(core): State<Arc<Core>>) -> Json<Value> {
    let mut tabs: Vec<_> = core.registry.tabs.iter().collect();
    // A bundle without a title is no tab: its module is loaded for `setup` only.
    tabs.sort_by_key(|t| (t.order, t.tab_id));
    let module = |t: &fgds_core::UiBundle| format!("/ui/plugins/{}/{}", t.tab_id, t.module);
    Json(tabs.iter().map(|t| json!({"id": t.tab_id, "title": t.title, "module": module(t)})).collect())
}

async fn plugin_file(State(core): State<Arc<Core>>, Path((tab, path)): Path<(String, String)>) -> Response {
    let tab = core.registry.tabs.iter().find(|t| t.tab_id == tab);
    match tab.and_then(|t| t.files.get(&path)) {
        Some(content) => file(&path, content),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
