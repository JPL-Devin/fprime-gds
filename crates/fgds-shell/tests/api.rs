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
        let raw = fgds_core::bytes::Bytes::copy_from_slice(packet);
        Ok(vec![NewRecord { id: Some(packet[0] as u32), raw, ..NewRecord::new("t.sample", body) }])
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
        reg.add_decoder(Arc::new(|_| Ok(Box::new(Decode))))?;
        reg.add_tab(UiBundle { tab_id: "test", title: Some("Test"), order: 1, module: "tab.js", files: &FILES })?;
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
    assert_eq!((status, seqs(&page)), (200, vec!["2", "3"]));
    assert_eq!(page["next"], "3");
    let (_, rest) = server.get("/api/v1/records?after_seq=3&limit=10").await;
    assert_eq!((seqs(&rest), &rest["next"]), (vec!["4", "5", "6"], &Value::Null));

    // API-4: 64-bit integers and non-finite floats as text, bytes as base64, UTC time.
    let record = &page["records"][0];
    assert_eq!(record["kind"], "t.sample");
    assert_eq!(record["body"], json!({"level": 1, "big": "18446744073709551615", "nan": "NaN"}));
    assert_eq!(record["raw"], "AQ==");
    let time = record["ground_time"].as_str().unwrap();
    assert!(time.len() == 27 && time.ends_with('Z') && time.as_bytes()[19] == b'.', "{time}");

    let (_, filtered) = server.get("/api/v1/records?filter=level:ge:4&id=4,5").await;
    assert_eq!(seqs(&filtered), ["5", "6"]);
    server.gds.receive(&[5]).await;
    let (_, latest) = server.get("/api/v1/records/latest?kind=t.sample").await;
    assert_eq!(seqs(&latest), ["2", "3", "4", "5", "7"]);
    assert_eq!(server.get("/api/v1/cursor").await.1, json!({"seq": "7", "run": 1}));
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
    assert_eq!(server.post("/api/v1/raw", json!({"data": "AQI="})).await, (200, json!({"sent": 2, "seq": "2"})));
    assert_eq!(server.gds.link.sent(), [vec![0, 2, 1, 2]]);
    assert_eq!(server.post("/api/v1/link", json!({"enabled": false})).await.0, 200);
    server.gds.wait_for_link("disabled").await;
    let (status, body) = server.post("/api/v1/raw", json!({"data": "AQI="})).await;
    assert_eq!((status, body["error"]["code"].as_str()), (503, Some("link_down")));
    // Both sends are recorded, the accepted one with the packet (SEC-5).
    let (_, page) = server.get("/api/v1/records?kind=raw.sent").await;
    let sent = &page["records"];
    assert_eq!(sent[0]["body"], json!({"length": 2, "client": "api", "outcome": "sent"}));
    assert_eq!(sent[0]["raw"], "AQI=");
    assert_eq!(sent[1]["body"]["outcome"], "rejected");
    // `in` and `prefix` filter operators.
    let (_, page) = server.get("/api/v1/records?kind=raw.sent&filter=outcome:in:failed,rejected").await;
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    let (_, page) = server.get("/api/v1/records?kind=raw.sent&filter=outcome:prefix:se").await;
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    server.post("/api/v1/link", json!({"enabled": true})).await;
    server.gds.wait_for_link("connected").await;
}

#[tokio::test]
async fn the_stream_replays_from_a_sequence_number_then_continues_live() {
    let server = Server::start().await;
    for level in 1..=3 {
        server.gds.receive(&[level]).await;
    }
    let url = format!("ws://{}/api/v1/stream?kind=t.sample&after_seq=2", server.address);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut seqs = Vec::new();
    while seqs.len() < 2 {
        seqs.push(next_seq(&mut socket).await);
    }
    assert_eq!(seqs, ["3", "4"]);
    // The replay has ended: told once, with the last sequence number sent.
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else { panic!("not text") };
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({"type": "live", "seq": "4"}));
    server.gds.receive(&[4]).await;
    server.gds.receive(&[5]).await;
    assert_eq!((next_seq(&mut socket).await, next_seq(&mut socket).await), ("5".into(), "6".into()));
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
    assert_eq!(headers["content-security-policy"], "default-src 'self'; connect-src 'self'; frame-ancestors 'none'");
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

#[tokio::test]
async fn errors_answered_by_the_framework_are_json_too() {
    let server = Server::start().await;
    let url = |path: &str| format!("http://{}{path}", server.address);
    let check = |response: reqwest::Response, status: u16, code: &'static str| async move {
        assert_eq!(response.status().as_u16(), status);
        let body: Value = response.json().await.expect("a JSON body");
        assert_eq!(body["error"]["code"], code);
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    };
    let post = || server.http.post(url("/api/v1/link"));
    // A body that is not JSON, a body of the wrong type, a wrong method, a body too large.
    let invalid = post().header("content-type", "application/json").body("{not json");
    check(invalid.send().await.unwrap(), 400, "bad_request").await;
    check(post().body("enabled=false").send().await.unwrap(), 415, "unsupported_media_type").await;
    check(server.http.get(url("/api/v1/link")).send().await.unwrap(), 405, "method_not_allowed").await;
    check(server.http.delete(url("/api/v1/records")).send().await.unwrap(), 405, "method_not_allowed").await;
    let big = post().json(&json!({"enabled": "x".repeat(2 * 1024 * 1024)}));
    check(big.send().await.unwrap(), 413, "payload_too_large").await;
    check(server.http.get(url("/api/v1/records?after_seq=-1")).send().await.unwrap(), 400, "bad_request").await;
}

#[tokio::test]
async fn a_failed_write_is_502_and_a_packet_too_large_for_the_frame_is_422() {
    let server = Server::start().await;
    server.gds.link.set_writes(fgds_core::testing::Writes::Fail);
    let (status, body) = server.post("/api/v1/raw", json!({"data": "AQI="})).await;
    assert_eq!((status, &body["error"]["code"]), (502, &json!("send_failed")));
    let (_, page) = server.get("/api/v1/records?kind=raw.sent").await;
    assert_eq!(page["records"][0]["body"]["outcome"], "failed");

    // Refused by the framing layer: nothing is written, the link stays up.
    server.gds.link.set_writes(fgds_core::testing::Writes::Accept);
    server.gds.wait_for_link("connected").await;
    let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, vec![0u8; 70_000]);
    let (status, body) = server.post("/api/v1/raw", json!({"data": data})).await;
    assert_eq!((status, &body["error"]["code"]), (422, &json!("invalid")));
    assert_eq!(body["error"]["message"], "packet of 70000 bytes does not fit the frame");
    let (_, page) = server.get("/api/v1/records?kind=raw.sent").await;
    assert_eq!(page["records"][1]["body"]["outcome"], "rejected");
    assert!(server.gds.link.sent().is_empty());
    assert_eq!(server.get("/api/v1/status").await.1["link"]["state"], "connected");
}

#[tokio::test]
async fn records_are_selected_by_sequence_time_run_and_repeated_kind() {
    let server = Server::start().await;
    for level in 1..=4 {
        server.gds.receive(&[level]).await;
    }
    let get = |query: &str| {
        let path = format!("/api/v1/records?{query}");
        let server = &server;
        async move {
            let (status, page) = server.get(&path).await;
            assert_eq!(status, 200, "{path}");
            page["records"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["seq"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(get("kind=t.sample&before_seq=4").await, ["2", "3"]);
    assert_eq!(get("kind=t.sample&kind=gds.status&limit=2").await, ["1", "2"]);
    assert_eq!(get("kind=t.sample&run=1&after_seq=4").await, ["5"]);
    assert_eq!(get("kind=t.sample&run=2").await, [] as [&str; 0]);
    // Ground time in microseconds: from inclusive, to exclusive.
    let now = fgds_core::Timestamp::now().0;
    assert_eq!(get(&format!("kind=t.sample&from={}&to={}", now - 60_000_000, now + 1)).await.len(), 4);
    assert_eq!(get(&format!("kind=t.sample&from={}", now + 1)).await.len(), 0);
    assert_eq!(get(&format!("kind=t.sample&to={}", now - 60_000_000)).await.len(), 0);
    assert_eq!(get("kind=t.sample&text=zzz").await, [] as [&str; 0]);
}

#[tokio::test]
async fn the_stream_refuses_another_origin() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let server = Server::start().await;
    let connect = |origin: String| {
        let mut request = format!("ws://{}/api/v1/stream", server.address).into_client_request().unwrap();
        request.headers_mut().insert("origin", origin.parse().unwrap());
        tokio_tungstenite::connect_async(request)
    };
    let refused = connect("http://evil.example".into()).await.unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(response) = refused else { panic!("{refused:?}") };
    assert_eq!(response.status().as_u16(), 403);
    assert!(connect(format!("http://{}", server.address)).await.is_ok());
}

/// Publish `total` records of 2 kB while the client of `socket` does not read, then
/// read to the end of the connection: (records received, last message).
async fn flood(server: &Server, total: usize) -> (usize, Value) {
    let url = format!("ws://{}/api/v1/stream?kind=t.sample", server.address);
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let pad = "x".repeat(2000);
    for _ in 0..total {
        server.gds.core.records.publish(NewRecord::new("t.sample", json!({"pad": pad}))).await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let mut received = 0;
    let mut last = Value::Null;
    let mut last_record = String::new();
    let drain = async {
        while let Some(Ok(message)) = socket.next().await {
            if let Message::Text(text) = message {
                last = fgds_core::serde_json::from_str(&text).unwrap();
                if last["type"] == "record" {
                    received += 1;
                    last_record = last["record"]["seq"].as_str().unwrap().to_string();
                    last = json!({"type": "record"});
                }
            }
        }
    };
    let ended = tokio::time::timeout(std::time::Duration::from_secs(60), drain).await;
    let limit = fgds_shell::STREAM_SEND_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(ended.is_ok(), "the connection does not end: {received} records, last {last}, time-out {limit}");
    if last["type"] == "lagged" {
        assert_eq!(last["last_seq"], last_record.as_str());
    }
    (received, last)
}

#[tokio::test]
async fn a_slow_stream_client_is_told_it_lagged_and_one_that_does_not_read_is_dropped() {
    // More records than a subscriber may be behind, far more than the socket holds.
    // (The queue of a subscriber holds 16,384: the next power of two above 10,000.)
    let total = 40_000;
    let server = Server::start().await;
    // A client that reads again in time gets what was queued, then `lagged` with the
    // last sequence number it was sent, then the close (API-6).
    let (received, last) = flood(&server, total).await;
    assert!(received > 0 && received < total, "{received} records");
    assert_eq!(last["type"], "lagged", "{last}");

    // A client that does not read within the time-out is closed without the message.
    fgds_shell::STREAM_SEND_TIMEOUT_MS.store(200, std::sync::atomic::Ordering::Relaxed);
    // Fewer records than the subscriber queue, so the send time-out is what ends it.
    let total = 12_000;
    let (received, last) = flood(&server, total).await;
    assert!(received < total, "{received} records");
    assert_eq!(last["type"], "record", "{last}");
}
