//! The server shell: every HTTP request passes through here before plugin code runs.
//! It serves the generic record API, the WebSocket stream, the plugins' routes and the
//! page shell with the plugins' tab modules (sections 4.7 and 4.8 of the design).

mod api;
mod ui;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use fgds_core::{ApiError, Core, Error, Result};
use std::sync::Arc;
use tokio::net::TcpListener;

/// Version of the network API, the `v1` of the paths.
pub const API_VERSION: u32 = 1;

/// Only same-origin scripts and connections: nothing inline, nothing evaluated (UI-5).
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; connect-src 'self'; frame-ancestors 'none'";

/// The whole application: core routes, each plugin's routes under its prefix, the UI.
pub fn router(core: Arc<Core>) -> Router {
    let mut api = api::routes(core.clone());
    for (plugin, routes) in &core.registry.routes {
        let supervised = middleware::from_fn_with_state((core.clone(), plugin.clone()), contain);
        let plugin_routes = routes.router.clone().with_state(core.context(plugin)).layer(supervised);
        api = api.nest(&format!("/{}", routes.prefix), plugin_routes);
    }
    let not_found = || async { ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such route") };
    Router::new()
        .nest("/api/v1", api.fallback(not_found))
        .merge(ui::routes(core.clone()))
        .layer(DefaultBodyLimit::max(core.config.max_body_bytes))
        .layer(middleware::from_fn(security_headers))
}

/// Bind the configured address. Separate from `serve` so that a taken port stops startup.
pub async fn bind(core: &Core) -> Result<TcpListener> {
    let address = &core.config.bind;
    TcpListener::bind(address).await.map_err(|e| Error::Config(format!("cannot listen on {address}: {e}")))
}

/// Serve until `shutdown` completes.
pub async fn serve(
    core: Arc<Core>,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    axum::serve(listener, router(core)).with_graceful_shutdown(shutdown).await.map_err(Error::Io)
}

/// Run a plugin's route in its own task, so that a panic there becomes a 500 answer,
/// a `gds.plugin_fault` record and a fault in the status; the server keeps running (MNT-7).
async fn contain(State((core, plugin)): State<(Arc<Core>, String)>, request: Request, next: Next) -> Response {
    match tokio::spawn(next.run(request)).await {
        Ok(response) => response,
        Err(e) => {
            let message = match e.try_into_panic() {
                Ok(panic) => panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic without a message".into()),
                Err(_) => "route cancelled".into(),
            };
            fgds_core::report_fault(&core.records, &core.status, &plugin, &message).await;
            let message = format!("plugin {plugin} failed: {message}");
            axum::response::IntoResponse::into_response(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "plugin_fault",
                message,
            ))
        }
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}
