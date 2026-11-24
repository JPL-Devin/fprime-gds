//! The public API of a running GDS, as the prompt uses it.

use fgds_core::serde_json::{self, Value};
use fgds_core::{Error, Result};
use futures_util::StreamExt;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// The API version the prompt speaks.
const API_VERSION: u64 = 1;

pub struct Api {
    base: String,
    http: reqwest::Client,
    client: String,
}

fn failed(what: &str, error: impl std::fmt::Display) -> Error {
    Error::Other(format!("{what}: {error}"))
}

/// A sequence number: the API writes them as text, being 64-bit.
pub fn seq(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

/// `--url` as `fgds command-send` reads it: the scheme and `/api/v1` may be left out.
pub fn base_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    let url = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
    if url.ends_with("/api/v1") {
        url
    } else {
        format!("{url}/api/v1")
    }
}

impl Api {
    /// `client` is the name the GDS records with what this prompt sends.
    pub fn new(base: String, client: String) -> Api {
        // No connection is kept between requests: one kept across a restart of the GDS fails the
        // next command, and one made on another runtime is never answered.
        let http = reqwest::Client::builder().pool_max_idle_per_host(0).build().unwrap_or_default();
        Api { base, http, client }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn client(&self) -> &str {
        &self.client
    }

    async fn answer(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request
            .header("x-fgds-client", &self.client)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| failed(&format!("no GDS answers at {}", self.base), e))?;
        let status = response.status();
        let body: Value = response.json().await.map_err(|e| failed("the answer of the GDS is not JSON", e))?;
        if status.is_success() {
            return Ok(body);
        }
        let message = body["error"]["message"].as_str().unwrap_or("no message");
        Err(if status.is_client_error() {
            Error::Invalid(message.to_string())
        } else {
            Error::Other(format!("{status}: {message}"))
        })
    }

    pub async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        self.answer(self.http.get(format!("{}{path}", self.base)).query(query)).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.answer(self.http.post(format!("{}{path}", self.base)).json(body)).await
    }

    /// Refuses a GDS that speaks another API version, naming both (API-9).
    pub async fn check_version(&self) -> Result<()> {
        let version = self.get("/version", &[]).await?;
        match version["api"].as_u64() {
            Some(API_VERSION) => Ok(()),
            other => Err(Error::Other(format!(
                "this prompt speaks API version {API_VERSION}, the GDS at {} speaks version {}",
                self.base,
                other.map_or("unknown".to_string(), |v| v.to_string())
            ))),
        }
    }

    /// Live records of `kinds`, each once, in the order the GDS sends them. `*after` is the last
    /// one seen (`None`: live only). Returns when the stream ends; the caller connects again.
    pub async fn follow(&self, kinds: &str, after: &mut Option<u64>, on_record: &mut impl FnMut(&Value)) -> Result<()> {
        let from = after.map(|seq| format!("&after_seq={seq}")).unwrap_or_default();
        let url = format!("{}/stream?kind={kinds}{from}", self.base.replacen("http", "ws", 1));
        let (mut socket, _) =
            tokio_tungstenite::connect_async(&url).await.map_err(|e| failed(&format!("no stream at {url}"), e))?;
        loop {
            let text = match socket.next().await {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_))) | None => return Err(Error::Other("the GDS closed the stream".into())),
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(failed("the stream failed", e)),
            };
            let message: Value = serde_json::from_str(&text).map_err(|e| failed("the stream sent no JSON", e))?;
            let at = match message["type"].as_str() {
                Some("record") => {
                    on_record(&message["record"]);
                    seq(&message["record"]["seq"])
                }
                Some("live") => seq(&message["seq"]),
                // The GDS dropped this client for being slow: the caller connects again from here.
                Some("lagged") => {
                    *after = seq(&message["last_seq"]).max(*after);
                    return Ok(());
                }
                _ => None,
            };
            *after = at.max(*after);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_may_leave_out_scheme_and_path() {
        assert_eq!(base_url("127.0.0.1:5000"), "http://127.0.0.1:5000/api/v1");
        assert_eq!(base_url("http://gds:5000/"), "http://gds:5000/api/v1");
        assert_eq!(base_url("https://gds/api/v1"), "https://gds/api/v1");
    }
}
