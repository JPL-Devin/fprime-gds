//! The HTTP and WebSocket API over a core running on the test doubles.

use fgds_core::axum::routing::get;
use fgds_core::serde_json::{json, Value};
use fgds_core::testing::{empty_dictionary, FakeGds};
use fgds_core::*;
use futures_util::StreamExt;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

static FILES: StaticFiles = StaticFiles(&[("tab.js", b"export default {};")]);

struct Decode;

impl Decoder for Decode {
    fn matches(&self) -> PacketMatch {
        PacketMatch::default()
    }
    fn decode(&self, packet: &[u8], _: &PacketMeta, _: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        let body = json!({"level": packet[0], "big": u64::MAX.to_string(), "nan": "NaN"});
        Ok(vec![NewRecord { id: Some(packet[0] as u32), ..NewRecord::new("t.sample", body) }])
    }
}

struct TestPlugin;

impl Plugin for TestPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "test", version: "1", ..Default::default() }
    }
    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_kind(KindSpec {
            fields: &[FieldSpec { name: "level", pointer: "/level", ty: FieldType::Number }],
            ..KindSpec::new("t.sample", "a sample")
        })?;
        reg.add_decoder(Arc::new(Decode))?;
        reg.add_tab(UiBundle { tab_id: "test", title: "Test", order: 1, module: "tab.js", files: &FILES })?;
        let router = axum::Router::new()
            .route("/hello", get(|| async { "hello" }))
            .route("/panic", get(broken).post(|_: String| async { "posted" }));
        reg.add_routes(Routes { prefix: "test", router, access: vec![] })
    }
}

async fn broken() -> &'static str {
    panic!("route bug")
}

struct Server {
    gds: FakeGds,
    address: String,
    http: reqwest::Client,
}

impl Server {
    async fn start() -> Server {
        let gds = FakeGds::start(&[&TestPlugin], empty_dictionary()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let app = fgds_shell::router(gds.core.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Server { gds, address, http: reqwest::Client::new() }
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let response = self.http.get(format!("http://{}{path}", self.address)).send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self.http.post(format!("http://{}{path}", self.address)).json(&body).send().await.unwrap();
        (response.status().as_u16(), response.json().await.unwrap_or(Value::Null))
    }
}

fn seqs(page: &Value) -> Vec<&str> {
    page["records"].as_array().unwrap().iter().map(|r| r["seq"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn records_are_served_in_pages_with_the_api_encoding() {
    let server = Server::start().await;
    for level in 1..=5 {
        server.gds.receive(&[level]).await;
    }
    let (status, page) = server.get("/api/v1/records?kind=t.sample&limit=2").await;
    assert_eq!((status, seqs(&page)), (200, vec!["1", "2"]));
    assert_eq!(page["next"], "2");
    let (_, rest) = server.get("/api/v1/records?after_seq=2&limit=10").await;
    assert_eq!((seqs(&rest), &rest["next"]), (vec!["3", "4", "5"], &Value::Null));

    // API-4: 64-bit integers and non-finite floats as text, bytes as base64, UTC time.
    let record = &page["records"][0];
    assert_eq!(record["kind"], "t.sample");
    assert_eq!(record["body"], json!({"level": 1, "big": "18446744073709551615", "nan": "NaN"}));
    assert_eq!(record["raw"], "AQ==");
    let time = record["ground_time"].as_str().unwrap();
    assert!(time.len() == 27 && time.ends_with('Z') && time.as_bytes()[19] == b'.', "{time}");

    let (_, filtered) = server.get("/api/v1/records?filter=level:ge:4&id=4,5").await;
    assert_eq!(seqs(&filtered), ["4", "5"]);
    server.gds.receive(&[5]).await;
    let (_, latest) = server.get("/api/v1/records/latest?kind=t.sample").await;
    assert_eq!(seqs(&latest), ["1", "2", "3", "4", "6"]);
    assert_eq!(server.get("/api/v1/cursor").await.1, json!({"seq": "6", "run": 1}));
}

#[tokio::test]
async fn malformed_requests_get_a_json_4xx() {
    let server = Server::start().await;
    for path in [
        "/api/v1/records?limit=many",
        "/api/v1/records?kind=nope",
        "/api/v1/records?color=red",
        "/api/v1/records?filter=level",
        "/api/v1/records?filter=level:like:1",
        "/api/v1/records?filter=nope:eq:1",
        "/api/v1/stream?kind=nope",
        "/api/v1/stream?limit=1",
    ] {
        let (status, body) = server.get(path).await;
        assert_eq!(status, 400, "{path}");
        assert_eq!(body["error"]["code"], "bad_request", "{path}");
        assert!(body["error"]["message"].is_string());
    }
    assert_eq!(server.get("/api/v1/nope").await.1["error"]["code"], "not_found");
    assert_eq!(server.get("/api/v1/dictionary?run=9").await.0, 404);
    assert_eq!(server.post("/api/v1/link", json!({})).await.0, 400);
    assert_eq!(server.post("/api/v1/raw", json!({"data": "***"})).await.0, 400);
    // A body over the limit is refused before the plugin sees it.
    let big = json!("x".repeat(2 * 1024 * 1024));
    assert_eq!(server.post("/api/v1/test/panic", big).await.0, 413);
}

#[tokio::test]
async fn version_status_kinds_runs_and_dictionary_are_served() {
    let server = Server::start().await;
    let (_, version) = server.get("/api/v1/version").await;
    assert_eq!(version["api"], 1);
    assert!(version["plugins"].as_array().unwrap().contains(&json!({"name": "test", "version": "1"})));
    let (_, status) = server.get("/api/v1/status").await;
    assert_eq!((&status["link"]["state"], &status["storage"]["state"]), (&json!("connected"), &json!("ok")));
    let (_, kinds) = server.get("/api/v1/kinds").await;
    let ids: Vec<_> = kinds.as_array().unwrap().iter().map(|k| k["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"t.sample") && ids.contains(&"gds.plugin_fault"));
    let (_, runs) = server.get("/api/v1/runs").await;
    assert_eq!(runs[0]["run"], 1);
    assert_eq!(server.get("/api/v1/dictionary").await.1["commands"], json!([]));
}

#[tokio::test]
async fn the_link_is_switched_and_raw_bytes_are_sent() {
    let server = Server::start().await;
    assert_eq!(server.post("/api/v1/raw", json!({"data": "AQI="})).await, (200, json!({"sent": 2})));
    assert_eq!(server.gds.link.sent(), [vec![0, 2, 1, 2]]);
    assert_eq!(server.post("/api/v1/link", json!({"enabled": false})).await.0, 200);
    server.gds.wait_for_link("disabled").await;
    let (status, body) = server.post("/api/v1/raw", json!({"data": "AQI="})).await;
    assert_eq!((status, body["error"]["code"].as_str()), (503, Some("link_down")));
    server.post("/api/v1/link", json!({"enabled": true})).await;
    server.gds.wait_for_link("connected").await;
}

#[tokio::test]
async fn the_stream_replays_from_a_sequence_number_then_continues_live() {
    let server = Server::start().await;
    for level in 1..=3 {
        server.gds.receive(&[level]).await;
    }
    let url = format!("ws://{}/api/v1/stream?kind=t.sample&after_seq=1", server.address);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut seqs = Vec::new();
    while seqs.len() < 2 {
        seqs.push(next_seq(&mut socket).await);
    }
    assert_eq!(seqs, ["2", "3"]);
    server.gds.receive(&[4]).await;
    server.gds.receive(&[5]).await;
    assert_eq!((next_seq(&mut socket).await, next_seq(&mut socket).await), ("4".into(), "5".into()));
}

/// The sequence number of the next record on the stream.
async fn next_seq<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> String
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        if let Message::Text(text) = socket.next().await.expect("stream closed").expect("stream failed") {
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"], "record");
            return value["record"]["seq"].as_str().unwrap().to_string();
        }
    }
}
#[tokio::test]
async fn plugin_routes_tabs_and_the_page_shell_are_served() {
    let server = Server::start().await;
    let text = |path: &str| {
        let url = format!("http://{}{path}", server.address);
        async move {
            let response = reqwest::get(url).await.unwrap();
            let headers = response.headers().clone();
            (response.status().as_u16(), headers, response.text().await.unwrap())
        }
    };
    assert_eq!(text("/api/v1/test/hello").await.2, "hello");
    let (_, tabs) = server.get("/ui/tabs").await;
    assert_eq!(tabs, json!([{"id": "test", "title": "Test", "module": "/ui/plugins/test/tab.js"}]));
    let (status, headers, module) = text("/ui/plugins/test/tab.js").await;
    assert_eq!((status, module.as_str()), (200, "export default {};"));
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/javascript"));
    assert_eq!(text("/ui/plugins/test/nope.js").await.0, 404);
    let (status, headers, page) = text("/").await;
    assert!(status == 200 && page.contains("/ui/shell.js"));
    assert_eq!(headers["content-security-policy"], "default-src 'self'; connect-src 'self'");
    assert!(text("/ui/shell.js").await.2.contains("/ui/tabs"));
}

#[tokio::test]
async fn a_panicking_route_answers_500_and_the_server_keeps_running() {
    let server = Server::start().await;
    let (status, body) = server.get("/api/v1/test/panic").await;
    assert_eq!((status, body["error"]["code"].as_str()), (500, Some("plugin_fault")));
    let (_, faults) = server.get("/api/v1/records?kind=gds.plugin_fault").await;
    assert_eq!(faults["records"][0]["body"], json!({"plugin": "test", "message": "route bug"}));
    let (_, status) = server.get("/api/v1/status").await;
    assert_eq!(status["plugins"]["test"]["fault"], "route bug");
    assert_eq!(server.get("/api/v1/cursor").await.0, 200);
}
