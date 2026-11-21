//! The public API of a running GDS, as this command line uses it.

use fgds_core::serde_json::Value;
use fgds_core::{Error, Result};
use futures_util::StreamExt;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// The API version this command line speaks.
const API_VERSION: u64 = 1;
const PAGE: &str = "1000";

pub struct Api {
    base: String,
    http: reqwest::Client,
}

fn failed(what: &str, error: impl std::fmt::Display) -> Error {
    Error::Other(format!("{what}: {error}"))
}

/// A sequence number: the API writes them as text, being 64-bit.
pub fn seq(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

/// Base64 to hexadecimal.
pub fn hex(base64: &str) -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut bits, mut count, mut out) = (0u32, 0, String::new());
    for c in base64.bytes().filter(|c| *c != b'=') {
        let value =
            ALPHABET.iter().position(|a| *a == c).ok_or_else(|| Error::Other("the packet is not base64".into()))?;
        bits = (bits << 6) | value as u32;
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push_str(&format!("{:02x}", (bits >> count) & 0xff));
        }
    }
    Ok(out)
}

impl Api {
    pub fn new(base: String) -> Api {
        Api { base, http: reqwest::Client::new() }
    }

    async fn answer(&self, request: reqwest::RequestBuilder) -> Result<Value> {
        let response = request
            .header("x-fgds-client", "fgds-cli")
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
        let code = body["error"]["code"].as_str().unwrap_or("error");
        Err(if status.is_client_error() {
            Error::Invalid(format!("{code}: {message}"))
        } else {
            Error::Other(format!("{status}: {code}: {message}"))
        })
    }

    pub async fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value> {
        self.answer(self.http.get(format!("{}{path}", self.base)).query(query)).await
    }

    pub async fn post(&self, path: &str, query: &[(&str, &str)], body: &Value) -> Result<Value> {
        self.answer(self.http.post(format!("{}{path}", self.base)).query(query).json(body)).await
    }

    /// Refuses a GDS that speaks another API version, naming both (API-9).
    pub async fn check_version(&self) -> Result<()> {
        let version = self.get("/version", &[]).await?;
        match version["api"].as_u64() {
            Some(API_VERSION) => Ok(()),
            other => Err(Error::Other(format!(
                "this command line speaks API version {API_VERSION}, the GDS at {} speaks version {}",
                self.base,
                other.map_or("unknown".to_string(), |v| v.to_string())
            ))),
        }
    }

    /// Stored records of `kind` after `*after`, up to `until` when given; `*after` moves along.
    pub async fn history(
        &self,
        kind: &str,
        after: &mut u64,
        until: Option<u64>,
        on_record: &mut impl FnMut(&Value) -> bool,
    ) -> Result<()> {
        loop {
            let from = after.to_string();
            let page = self.get("/records", &[("kind", kind), ("after_seq", &from), ("limit", PAGE)]).await?;
            for record in page["records"].as_array().into_iter().flatten() {
                let Some(at) = seq(&record["seq"]) else { continue };
                if until.is_some_and(|until| at > until) {
                    return Ok(());
                }
                *after = at;
                on_record(record);
            }
            match seq(&page["next"]) {
                Some(next) => *after = (*after).max(next),
                None => return Ok(()),
            }
        }
    }

    /// Records of `kind` after `*after`, stored ones first and then live ones, each
    /// once and in order (API-6). Returns after the first accepted record when
    /// `first_only`; otherwise only on an error. Reconnects where it stopped when the
    /// GDS drops this client for being slow.
    pub async fn follow(
        &self,
        kind: &str,
        after: &mut u64,
        first_only: bool,
        on_record: &mut impl FnMut(&Value) -> bool,
    ) -> Result<()> {
        let base = self.base.replacen("http", "ws", 1);
        loop {
            let url = format!("{base}/stream?kind={kind}&after_seq={after}");
            let (mut socket, _) =
                tokio_tungstenite::connect_async(&url).await.map_err(|e| failed(&format!("no stream at {url}"), e))?;
            loop {
                let text = match socket.next().await {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => return Err(Error::Other("the GDS closed the stream".into())),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(failed("the stream failed", e)),
                };
                let message: Value =
                    fgds_core::serde_json::from_str(&text).map_err(|e| failed("the stream sent no JSON", e))?;
                match message["type"].as_str() {
                    Some("record") => {
                        let record = &message["record"];
                        // A record seen before a reconnection is not printed twice.
                        match seq(&record["seq"]) {
                            Some(at) if at > *after => *after = at,
                            _ => continue,
                        }
                        if on_record(record) && first_only {
                            return Ok(());
                        }
                    }
                    Some("lagged") => {
                        *after = seq(&message["last_seq"]).map_or(*after, |last| last.max(*after));
                        break;
                    }
                    // Other notices (such as the end of the replay) need no action here.
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgds_core::serde_json::json;

    #[test]
    fn base64_packets_print_as_hexadecimal() {
        assert_eq!(hex("AAABAAAA").unwrap(), "000001000000");
        assert_eq!(hex("3q2+7w==").unwrap(), "deadbeef");
        assert_eq!(hex("3q2+").unwrap(), "deadbe");
        assert!(hex("*").is_err());
    }

    #[test]
    fn sequence_numbers_are_text_or_numbers() {
        assert_eq!(seq(&json!("18446744073709551615")), Some(u64::MAX));
        assert_eq!(seq(&json!(7)), Some(7));
        assert_eq!(seq(&json!(null)), None);
    }
}
