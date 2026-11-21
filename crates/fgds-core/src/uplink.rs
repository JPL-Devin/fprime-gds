use crate::{Encoded, Encoder, Link, LinkError, NewRecord, Records};
use base64::Engine;
use fgds_dictionary::Dictionary;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Why a send did not happen. The HTTP status of each case is fixed by section 4.5.
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum SendError {
    /// No encoder is registered for the request kind (404).
    #[error("unknown request kind \"{0}\"")]
    UnknownKind(String),
    /// The request failed validation against the dictionary (422).
    #[error("{0}")]
    Invalid(String),
    /// The link is down or disabled; nothing is queued (503, LNK-11).
    #[error("link down")]
    LinkDown,
    /// The write failed or timed out (502).
    #[error("{0}")]
    Failed(String),
}

/// A send that reached the link.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    /// Sequence number of the record of this send.
    pub seq: u64,
    /// Per-send identifier; later records about this send carry the same entity (LNK-23).
    pub entity: String,
}

struct Inner {
    encoders: HashMap<&'static str, Arc<dyn Encoder>>,
    dict: Arc<Dictionary>,
    link: Link,
    records: Records,
    run: u32,
    sends: AtomicU64,
}

/// The one path to the flight link: validate, send, record (section 4.5). Cheap to clone.
#[derive(Clone)]
pub struct Uplink(Arc<Inner>);

impl Uplink {
    pub fn new(
        encoders: &[Arc<dyn Encoder>],
        dict: Arc<Dictionary>,
        link: Link,
        records: Records,
        run: u32,
    ) -> Uplink {
        Uplink(Arc::new(Inner {
            encoders: encoders.iter().map(|e| (e.request_kind(), e.clone())).collect(),
            dict,
            link,
            records,
            run,
            sends: AtomicU64::new(0),
        }))
    }

    fn encoder(&self, request_kind: &str) -> Result<&Arc<dyn Encoder>, SendError> {
        self.0
            .encoders
            .get(request_kind)
            .ok_or_else(|| SendError::UnknownKind(request_kind.to_string()))
    }

    /// Validate and encode only: nothing is sent or recorded (LNK-21).
    pub fn dry_run(&self, request_kind: &str, request: &Value) -> Result<Encoded, SendError> {
        self.encoder(request_kind)?
            .encode(request, &self.0.dict)
            .map_err(|e| SendError::Invalid(e.0))
    }

    /// Validate, send and record one request. Every outcome but an unknown request
    /// kind is recorded, with `outcome` `sent`, `rejected` or `failed`.
    pub async fn send(
        &self,
        request_kind: &str,
        request: &Value,
        client: &str,
    ) -> Result<Sent, SendError> {
        let kind = self.encoder(request_kind)?.record_kind();
        let entity = format!("{}-{}", self.0.run, self.0.sends.fetch_add(1, Ordering::SeqCst) + 1);
        let (encoded, result) = match self.dry_run(request_kind, request) {
            Err(e) => (None, Err(e)),
            Ok(encoded) => {
                let result = self.0.link.write_packet(&encoded.packet).await.map_err(|e| match e {
                    LinkError::Down => SendError::LinkDown,
                    LinkError::Failed(reason) => SendError::Failed(reason),
                });
                (Some(encoded), result)
            }
        };
        let mut body = match &encoded {
            Some(encoded) => encoded.body.clone(),
            None => json!({"request": request}),
        };
        body["client"] = json!(client);
        match &result {
            Ok(link_bytes) => {
                body["outcome"] = json!("sent");
                body["link_bytes"] = json!(base64::engine::general_purpose::STANDARD.encode(link_bytes));
            }
            Err(e) => {
                let rejected = matches!(e, SendError::Invalid(_) | SendError::LinkDown);
                body["outcome"] = json!(if rejected { "rejected" } else { "failed" });
                body["reason"] = json!(e.to_string());
            }
        }
        let record = NewRecord {
            id: encoded.as_ref().and_then(|e| e.id),
            entity: Some(entity.clone()),
            raw: encoded.map(|e| e.packet).unwrap_or_default(),
            ..NewRecord::new(kind, body)
        };
        let seq = self.0.records.publish(record).await.map_err(|e| SendError::Failed(e.to_string()));
        result.and(seq).map(|seq| Sent { seq, entity })
    }

    /// Send an already encoded packet (file uplink); not recorded here.
    pub async fn send_packet(&self, packet: &[u8]) -> Result<(), SendError> {
        match self.0.link.write_packet(packet).await {
            Ok(_) => Ok(()),
            Err(LinkError::Down) => Err(SendError::LinkDown),
            Err(LinkError::Failed(reason)) => Err(SendError::Failed(reason)),
        }
    }
}
