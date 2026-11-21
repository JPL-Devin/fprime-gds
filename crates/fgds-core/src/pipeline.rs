use crate::{
    Error, Filter, KindSpec, NewRecord, Page, Query, Record, Result, RunInfo, Status, Storage, Timestamp,
    KIND_STORAGE_GAP, KIND_STORAGE_RECOVERED,
};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, Notify};

/// Records waiting for storage. When this many wait, publishers (and so a TCP link) wait.
const QUEUE: usize = 1_000;
/// Largest batch handed to storage in one append.
const BATCH: usize = 500;
/// Live records a subscriber may fall behind by before it lags (API-6).
const SUBSCRIBER_QUEUE: usize = 10_000;
/// Default and largest page of a history query (API-7).
pub(crate) const DEFAULT_LIMIT: u32 = 1_000;
pub(crate) const MAX_LIMIT: u32 = 10_000;
/// Sequence numbers reserved in storage ahead of use (STO-14).
const RESERVE_BLOCK: u64 = 1_000_000;

/// Limits of the pipeline.
#[derive(Debug, Clone, Copy)]
pub struct PipelineOptions {
    /// A commit that takes longer puts storage in state `failed` (STO-9).
    pub commit_timeout: Duration,
    /// No record is accepted: the storage was opened read-only (STO-12).
    pub read_only: bool,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        PipelineOptions { commit_timeout: Duration::from_secs(2), read_only: false }
    }
}

struct State {
    next_seq: u64,
    /// The hand-off queue, in `seq` order.
    pending: VecDeque<Record>,
    /// Records of the uplink service and of the core itself: taken first, never waiting.
    uplink: VecDeque<Record>,
    flushes: Vec<oneshot::Sender<()>>,
    /// Storage is failed: publishers do not wait and records are broadcast unstored.
    failed: bool,
    closed: bool,
}

struct Inner {
    storage: Arc<dyn Storage>,
    kinds: HashMap<String, KindSpec>,
    run: u32,
    options: PipelineOptions,
    state: Mutex<State>,
    /// Wakes the commit task.
    work: Notify,
    /// Wakes publishers waiting for room in the queue.
    space: Notify,
    live: broadcast::Sender<Arc<Record>>,
    /// Highest sequence number broadcast.
    cursor: AtomicU64,
    /// Highest sequence number reserved in storage.
    reserved: AtomicU64,
    status: Status,
}

/// The record pipeline: number, store, then broadcast (section 4.2). Cheap to clone.
#[derive(Clone)]
pub struct Records(Arc<Inner>);

impl Records {
    /// Start the pipeline on an opened storage, with the default limits.
    pub fn start(storage: Arc<dyn Storage>, run: &RunInfo, kinds: &[KindSpec], status: Status) -> Self {
        Self::start_with(storage, run, kinds, status, PipelineOptions::default())
    }

    pub fn start_with(
        storage: Arc<dyn Storage>,
        run: &RunInfo,
        kinds: &[KindSpec],
        status: Status,
        options: PipelineOptions,
    ) -> Self {
        let (live, _) = broadcast::channel(SUBSCRIBER_QUEUE);
        let records = Records(Arc::new(Inner {
            storage,
            kinds: kinds.iter().map(|k| (k.id.clone(), k.clone())).collect(),
            run: run.run,
            options,
            state: Mutex::new(State {
                next_seq: run.first_seq,
                pending: VecDeque::new(),
                uplink: VecDeque::new(),
                flushes: Vec::new(),
                failed: false,
                closed: options.read_only,
            }),
            work: Notify::new(),
            space: Notify::new(),
            live,
            cursor: AtomicU64::new(run.first_seq.saturating_sub(1)),
            reserved: AtomicU64::new(run.reserved_seq),
            status,
        }));
        records.0.status.set_storage(if options.read_only { "read-only" } else { "ok" });
        if !options.read_only {
            tokio::spawn(records.clone().commit_loop());
        }
        records
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock only means a panic elsewhere; the data is still usable.
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Store each batch, then broadcast it: a record is never live before it is stored,
    /// unless storage is failed (STO-9). While storage is failed, every batch is still
    /// offered to it, which is the retry; records it refuses are broadcast with
    /// `stored = false` and reported as a gap when it accepts records again.
    async fn commit_loop(self) {
        // First and last sequence number not stored.
        let mut gap: Option<(u64, u64)> = None;
        loop {
            let batch: Vec<Record> = {
                let mut state = self.state();
                // Uplink records first; the batch is put back in `seq` order below.
                let n = state.uplink.len().min(BATCH);
                let mut batch: Vec<Record> = state.uplink.drain(..n).collect();
                let n = state.pending.len().min(BATCH - batch.len());
                batch.extend(state.pending.drain(..n));
                if batch.is_empty() {
                    for done in state.flushes.drain(..) {
                        let _ = done.send(());
                    }
                    if state.closed {
                        return;
                    }
                }
                batch
            };
            if batch.is_empty() {
                self.0.work.notified().await;
                continue;
            }
            self.0.space.notify_waiters();
            let transient = |r: &Record| self.0.kinds.get(&r.kind).is_some_and(|k| k.transient);
            let stored: Vec<Record> = batch.iter().filter(|r| !transient(r)).cloned().collect();
            let limit = self.0.options.commit_timeout;
            let result = match tokio::time::timeout(limit, self.0.storage.append(&stored)).await {
                Ok(result) => result,
                Err(_) => Err(Error::other(format!("commit did not finish in {} ms", limit.as_millis()))),
            };
            match (result, stored.first().zip(stored.last())) {
                (Ok(()), _) => {
                    self.broadcast(batch, |_| true);
                    if let Some((first, last)) = gap.take() {
                        self.recovered(first, last);
                    }
                    self.reserve_ahead().await;
                }
                (Err(e), range) => {
                    if gap.is_none() {
                        tracing::error!(error = %e, "storage failed: records are delivered unstored");
                    }
                    self.0.status.set_storage(&format!("failed: {e}"));
                    self.state().failed = true;
                    // Publishers no longer wait for room.
                    self.0.space.notify_waiters();
                    if let Some((first, last)) = range {
                        let lost = (
                            gap.map_or(first.seq, |g| g.0.min(first.seq)),
                            gap.map_or(last.seq, |g| g.1.max(last.seq)),
                        );
                        gap = Some(lost);
                        self.0.status.set_storage_gap(gap);
                    }
                    self.broadcast(batch, transient);
                }
            }
        }
    }

    /// Storage accepts records again: say so, and say what was not stored (STO-9).
    fn recovered(&self, first: u64, last: u64) {
        self.state().failed = false;
        tracing::error!(first_seq = first, last_seq = last, "storage recovered; these records were not stored");
        self.0.status.set_storage("ok");
        let lost = json!({"first_seq": first.to_string(), "last_seq": last.to_string()});
        let _ = self.enqueue(NewRecord::new(KIND_STORAGE_GAP, lost), true);
        let _ = self.enqueue(NewRecord::new(KIND_STORAGE_RECOVERED, json!({})), true);
    }

    /// Keep the reservation in storage ahead of the numbers handed out (STO-14).
    async fn reserve_ahead(&self) {
        let next = self.state().next_seq;
        if next + RESERVE_BLOCK / 2 <= self.0.reserved.load(Ordering::SeqCst) {
            return;
        }
        let through = next + RESERVE_BLOCK;
        match tokio::time::timeout(self.0.options.commit_timeout, self.0.storage.reserve(through)).await {
            Ok(Ok(())) => self.0.reserved.store(through, Ordering::SeqCst),
            Ok(Err(e)) => tracing::warn!(error = %e, "sequence numbers could not be reserved"),
            Err(_) => tracing::warn!("sequence numbers could not be reserved: time-out"),
        }
    }

    fn broadcast(&self, mut batch: Vec<Record>, stored: impl Fn(&Record) -> bool) {
        self.0.status.count("records", batch.len() as u64);
        batch.sort_by_key(|r| r.seq);
        for mut record in batch {
            record.stored = stored(&record);
            self.0.cursor.fetch_max(record.seq, Ordering::SeqCst);
            // An error only means there is no subscriber.
            let _ = self.0.live.send(Arc::new(record));
        }
    }

    /// Publish a record. Returns its sequence number, assigned now; storage follows.
    /// Waits while the hand-off queue is full, which makes a TCP link wait (section 4.2).
    pub async fn publish(&self, new: NewRecord) -> Result<u64> {
        loop {
            let space = self.0.space.notified();
            tokio::pin!(space);
            // Registered before the check, so a wake-up between check and wait is not lost.
            space.as_mut().enable();
            let full = {
                let state = self.state();
                !state.failed && !state.closed && state.pending.len() >= QUEUE
            };
            if !full {
                return self.enqueue(new, false);
            }
            space.await;
        }
    }

    /// Publish a record of the uplink service: its queue is drained first and it never
    /// waits, so commanding does not wait behind telemetry (LNK-11).
    pub fn publish_uplink(&self, new: NewRecord) -> Result<u64> {
        self.enqueue(new, true)
    }

    fn enqueue(&self, new: NewRecord, uplink: bool) -> Result<u64> {
        if !self.0.kinds.contains_key(&new.kind) {
            return Err(Error::Invalid(format!("record kind \"{}\" is not registered", new.kind)));
        }
        let seq = {
            let mut state = self.state();
            if state.closed {
                let why = if self.0.options.read_only { "the storage is read-only" } else { "pipeline stopped" };
                return Err(Error::other(why));
            }
            let seq = state.next_seq;
            state.next_seq += 1;
            let record = Record {
                seq,
                run: self.0.run,
                ground_time: Timestamp::now(),
                kind: new.kind,
                flight_time: new.flight_time,
                id: new.id,
                entity: new.entity,
                stored: true,
                raw: new.raw,
                meta: new.meta,
                body: new.body,
            };
            match uplink {
                true => state.uplink.push_back(record),
                false => state.pending.push_back(record),
            }
            seq
        };
        self.0.work.notify_one();
        if seq == self.0.reserved.load(Ordering::SeqCst).saturating_add(1) {
            tracing::warn!(seq, "sequence numbers are used beyond the reservation in storage");
            self.0.status.warn("sequence numbers are used beyond the reservation in storage");
        }
        Ok(seq)
    }

    /// Wait until every record published so far is broadcast (and stored, unless storage is failed).
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        self.state().flushes.push(done);
        self.0.work.notify_one();
        let _ = wait.await;
    }

    /// Stop accepting records, flush, and give back the unused reservation so the next
    /// run continues at the next number.
    pub async fn stop(&self) {
        let (next, failed) = {
            let mut state = self.state();
            if state.closed {
                return;
            }
            state.closed = true;
            (state.next_seq, state.failed)
        };
        self.flush().await;
        if !failed {
            let released = tokio::time::timeout(self.0.options.commit_timeout, self.0.storage.reserve(next - 1));
            if !matches!(released.await, Ok(Ok(()))) {
                tracing::warn!("the sequence number reservation was not released");
            }
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
            from: from_seq.unwrap_or(0),
            replaying: from_seq.is_some(),
            caught_up: from_seq.is_none(),
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

    /// Highest sequence number broadcast so far.
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

    /// The dictionary JSON of a run, when the storage keeps it (STO-4).
    pub async fn dictionary(&self, run: u32) -> Result<Option<Vec<u8>>> {
        self.0.storage.dictionary(run).await
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

/// What a subscription delivers.
#[derive(Debug)]
pub enum StreamItem {
    Record(Box<Record>),
    /// The replay has ended: every later record is live. `seq` is the last record delivered.
    Live {
        seq: u64,
    },
}

/// A subscription: replay from storage, then live records.
pub struct RecordStream {
    records: Records,
    filter: Filter,
    live: broadcast::Receiver<Arc<Record>>,
    /// Sequence number of the last record seen; nothing at or below it is delivered again.
    last: u64,
    /// Where the subscription started: a gap record about earlier records is not for it.
    from: u64,
    replaying: bool,
    /// The replay ended since `take_caught_up` was last called.
    caught_up: bool,
    replay: VecDeque<Record>,
}

impl RecordStream {
    /// The next record, or `Live` once when the replay from storage has ended (at once
    /// for a subscription without a starting point). A subscriber that lagged is reported.
    pub async fn next_item(&mut self) -> Result<StreamItem, StreamEnd> {
        loop {
            if let Some(record) = self.replay.pop_front() {
                self.last = self.last.max(record.seq);
                return Ok(StreamItem::Record(Box::new(record)));
            }
            if std::mem::take(&mut self.caught_up) {
                return Ok(StreamItem::Live { seq: self.last });
            }
            if self.replaying {
                // The live receiver exists already, so a record committed while this
                // query runs is seen there and dropped below if the query returned it too.
                let mut kinds = self.filter.kinds.clone();
                if !kinds.is_empty() && !kinds.iter().any(|k| k == KIND_STORAGE_GAP) {
                    // A replay that crosses a gap is told what it missed (STO-9).
                    kinds.push(KIND_STORAGE_GAP.to_string());
                }
                // Every run: a client continues across a restart of the GDS (API-5).
                let query = Query { kinds, after_seq: Some(self.last), all_runs: true, ..Default::default() };
                match self.records.query(query).await {
                    Ok(page) if !page.records.is_empty() => {
                        let end = page.records.last().map_or(0, |r| r.seq);
                        let (filter, from) = (&self.filter, self.from);
                        self.replay.extend(page.records.into_iter().filter(|r| filter.matches(r) || covers(r, from)));
                        // Nothing may have passed the filter: continue after this page.
                        self.last = self.last.max(end);
                    }
                    Ok(_) => {
                        self.replaying = false;
                        self.caught_up = true;
                    }
                    Err(_) => return Err(StreamEnd::Closed),
                }
                continue;
            }
            match self.live.recv().await {
                Ok(record) if record.seq > self.last && self.filter.matches(&record) => {
                    self.last = record.seq;
                    return Ok(StreamItem::Record(Box::new((*record).clone())));
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return Err(StreamEnd::Lagged { last_seq: self.last }),
                Err(broadcast::error::RecvError::Closed) => return Err(StreamEnd::Closed),
            }
        }
    }

    /// The next record; a subscriber that lagged is reported instead of resumed (API-6).
    pub async fn next_strict(&mut self) -> Result<Record, StreamEnd> {
        loop {
            if let StreamItem::Record(record) = self.next_item().await? {
                return Ok(*record);
            }
        }
    }

    /// The next record; a subscriber that lagged resumes from storage at its last record.
    pub async fn next(&mut self) -> Option<Record> {
        loop {
            match self.next_strict().await {
                // A handler gets its own kinds only; a gap is in the status and the log.
                Ok(record) if !self.filter.matches(&record) => {}
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

/// True for a gap record about records after `from`.
fn covers(record: &Record, from: u64) -> bool {
    let last = record.body["last_seq"].as_str().and_then(|s| s.parse::<u64>().ok());
    record.kind == KIND_STORAGE_GAP && last.is_some_and(|last| last > from)
}
