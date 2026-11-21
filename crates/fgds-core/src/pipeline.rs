use crate::{Error, Filter, KindSpec, NewRecord, Page, Query, Record, Result, RunInfo, Status, Storage, Timestamp};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};

/// Records waiting for storage; a full queue makes publishers (and so the link) wait.
const QUEUE: usize = 1_000;
/// Largest batch handed to storage in one append.
const BATCH: usize = 500;
/// Live records a subscriber may fall behind by before it lags (API-6).
const SUBSCRIBER_QUEUE: usize = 10_000;
/// Default and largest page of a history query (API-7).
pub(crate) const DEFAULT_LIMIT: u32 = 1_000;
pub(crate) const MAX_LIMIT: u32 = 10_000;

enum Message {
    Record(Box<Record>),
    Flush(oneshot::Sender<()>),
}

struct Inner {
    storage: Arc<dyn Storage>,
    kinds: HashMap<String, KindSpec>,
    run: u32,
    /// Next sequence number and the queue, locked together so queue order is `seq` order.
    queue: Mutex<(u64, mpsc::Sender<Message>)>,
    live: broadcast::Sender<Arc<Record>>,
    /// Highest committed sequence number.
    cursor: AtomicU64,
}

/// The record pipeline: number, store, then broadcast (section 4.2). Cheap to clone.
#[derive(Clone)]
pub struct Records(Arc<Inner>);

impl Records {
    /// Start the pipeline on an opened storage.
    pub fn start(storage: Arc<dyn Storage>, run: &RunInfo, kinds: &[KindSpec], status: Status) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (live, _) = broadcast::channel(SUBSCRIBER_QUEUE);
        let records = Records(Arc::new(Inner {
            storage,
            kinds: kinds.iter().map(|k| (k.id.clone(), k.clone())).collect(),
            run: run.run,
            queue: Mutex::new((run.first_seq, tx)),
            live,
            cursor: AtomicU64::new(run.first_seq.saturating_sub(1)),
        }));
        status.set_storage("ok");
        tokio::spawn(records.clone().commit_loop(rx, status));
        records
    }

    /// Store each batch, then broadcast it: a record is never live before it is stored.
    async fn commit_loop(self, mut rx: mpsc::Receiver<Message>, status: Status) {
        let mut batch = Vec::new();
        while rx.recv_many(&mut batch, BATCH).await > 0 {
            let mut records = Vec::new();
            let mut flushes = Vec::new();
            for message in batch.drain(..) {
                match message {
                    Message::Record(r) => records.push(*r),
                    Message::Flush(done) => flushes.push(done),
                }
            }
            let stored: Vec<Record> = records.iter().filter(|r| r.stored).cloned().collect();
            let failed = match self.0.storage.append(&stored).await {
                Ok(()) => false,
                Err(e) => {
                    tracing::error!(error = %e, "storage append failed");
                    status.set_storage(&format!("failed: {e}"));
                    true
                }
            };
            for mut record in records {
                record.stored &= !failed;
                self.0.cursor.store(record.seq, Ordering::SeqCst);
                status.count("records", 1);
                // An error only means there is no subscriber.
                let _ = self.0.live.send(Arc::new(record));
            }
            for done in flushes {
                let _ = done.send(());
            }
        }
    }

    /// Publish a record. Returns its sequence number, assigned now; storage follows.
    pub async fn publish(&self, new: NewRecord) -> Result<u64> {
        let Some(spec) = self.0.kinds.get(&new.kind) else {
            return Err(Error::Invalid(format!("record kind \"{}\" is not registered", new.kind)));
        };
        let mut queue = self.0.queue.lock().await;
        let seq = queue.0;
        let record = Record {
            seq,
            run: self.0.run,
            ground_time: Timestamp::now(),
            kind: new.kind,
            flight_time: new.flight_time,
            id: new.id,
            entity: new.entity,
            stored: !spec.transient,
            raw: new.raw,
            meta: new.meta,
            body: new.body,
        };
        queue.1.send(Message::Record(Box::new(record))).await.map_err(|_| Error::other("pipeline stopped"))?;
        queue.0 += 1;
        Ok(seq)
    }

    /// Wait until every record published so far is stored and broadcast.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        let sent = self.0.queue.lock().await.1.send(Message::Flush(done)).await;
        if sent.is_ok() {
            let _ = wait.await;
        }
    }

    /// Records matching `filter`: those stored after `from_seq` first, then live ones,
    /// none repeated and none missing (API-5). `None` starts with the next live record.
    pub fn subscribe(&self, filter: Filter, from_seq: Option<u64>) -> RecordStream {
        RecordStream {
            live: self.0.live.subscribe(),
            records: self.clone(),
            filter,
            last: from_seq.unwrap_or(0),
            replaying: from_seq.is_some(),
            replay: VecDeque::new(),
        }
    }

    fn checked(&self, mut q: Query) -> Result<Query> {
        if q.limit == 0 {
            q.limit = DEFAULT_LIMIT;
        }
        q.limit = q.limit.min(MAX_LIMIT);
        let kinds: Vec<KindSpec> = self.0.kinds.values().cloned().collect();
        q.validate(&kinds)?;
        Ok(q)
    }

    /// One page of history. A `limit` of 0 means the default page size.
    pub async fn query(&self, q: Query) -> Result<Page> {
        self.0.storage.query(&self.checked(q)?).await
    }

    /// Current state: the latest matching record per entity or identifier.
    pub async fn latest(&self, q: Query) -> Result<Page> {
        self.0.storage.latest(&self.checked(q)?).await
    }

    /// Highest sequence number stored so far.
    pub fn cursor(&self) -> u64 {
        self.0.cursor.load(Ordering::SeqCst)
    }

    pub fn kinds(&self) -> Vec<KindSpec> {
        let mut kinds: Vec<_> = self.0.kinds.values().cloned().collect();
        kinds.sort_by(|a, b| a.id.cmp(&b.id));
        kinds
    }

    pub async fn runs(&self) -> Result<Vec<RunInfo>> {
        self.0.storage.runs().await
    }
}

/// Why a strict stream ended.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamEnd {
    Closed,
    /// The subscriber was too slow; `last_seq` is the last record it received.
    Lagged {
        last_seq: u64,
    },
}

/// A subscription: replay from storage, then live records.
pub struct RecordStream {
    records: Records,
    filter: Filter,
    live: broadcast::Receiver<Arc<Record>>,
    /// Sequence number of the last record seen; nothing at or below it is delivered again.
    last: u64,
    replaying: bool,
    replay: VecDeque<Record>,
}

impl RecordStream {
    /// The next record; a subscriber that lagged is reported instead of resumed (API-6).
    pub async fn next_strict(&mut self) -> Result<Record, StreamEnd> {
        loop {
            if let Some(record) = self.replay.pop_front() {
                self.last = record.seq;
                return Ok(record);
            }
            if self.replaying {
                // The live receiver exists already, so a record committed while this
                // query runs is seen there and dropped below if the query returned it too.
                let query =
                    Query { kinds: self.filter.kinds.clone(), after_seq: Some(self.last), ..Default::default() };
                match self.records.query(query).await {
                    Ok(page) if !page.records.is_empty() => self.replay.extend(page.records),
                    Ok(_) => self.replaying = false,
                    Err(_) => return Err(StreamEnd::Closed),
                }
                continue;
            }
            match self.live.recv().await {
                Ok(record) if record.seq > self.last && self.filter.matches(&record) => {
                    self.last = record.seq;
                    return Ok((*record).clone());
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return Err(StreamEnd::Lagged { last_seq: self.last }),
                Err(broadcast::error::RecvError::Closed) => return Err(StreamEnd::Closed),
            }
        }
    }

    /// The next record; a subscriber that lagged resumes from storage at its last record.
    pub async fn next(&mut self) -> Option<Record> {
        loop {
            match self.next_strict().await {
                Ok(record) => return Some(record),
                Err(StreamEnd::Closed) => return None,
                Err(StreamEnd::Lagged { .. }) => {
                    self.live = self.live.resubscribe();
                    self.replaying = true;
                }
            }
        }
    }
}
