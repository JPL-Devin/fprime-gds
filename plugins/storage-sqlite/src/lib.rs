//! Storage `sqlite`: every run of a database file, with the dictionary of each run
//! (sections 4.2 and 4.6 of the design, requirements STO).
//!
//! One connection writes and one reads, both on blocking threads, so a slow disk never
//! holds a runtime thread and a query does not wait for a commit. The journal is the
//! write-ahead log: a batch is durable against a killed process once `append` returns.

use fgds_core::bytes::Bytes;
use fgds_core::serde_json::{self, Value};
use fgds_core::{
    async_trait, Error, FlightTime, KindSpec, Manifest, MetaValue, PacketMeta, Page, Plugin, Query, Record, Registry,
    Result, RunInfo, Storage, StorageConfig, Timestamp,
};
use rusqlite::types::Value as Sql;
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Version of the tables below. A database with a higher one is refused.
const SCHEMA_VERSION: i64 = 1;
/// Sequence numbers reserved when a run begins (STO-14); the pipeline reserves more.
const RESERVE_BLOCK: u64 = 1_000_000;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS dictionaries (hash TEXT PRIMARY KEY, json BLOB NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS runs (
    run INTEGER PRIMARY KEY,
    started INTEGER NOT NULL,
    gds_version TEXT NOT NULL,
    dictionary_version TEXT NOT NULL,
    dictionary_hash TEXT NOT NULL,
    plugins TEXT NOT NULL,
    first_seq INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS records (
    seq INTEGER PRIMARY KEY,
    run INTEGER NOT NULL,
    ground_time INTEGER NOT NULL,
    kind TEXT NOT NULL,
    id INTEGER,
    entity TEXT,
    flight_base INTEGER,
    flight_context INTEGER,
    flight_seconds INTEGER,
    flight_microseconds INTEGER,
    raw BLOB NOT NULL,
    meta TEXT NOT NULL,
    body TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS records_kind ON records (kind, seq);
CREATE INDEX IF NOT EXISTS records_kind_id ON records (kind, id, seq);
CREATE INDEX IF NOT EXISTS records_time ON records (ground_time);
";

const COLUMNS: &str = "seq, run, ground_time, kind, id, entity, flight_base, flight_context, flight_seconds, \
                       flight_microseconds, raw, meta, body";

pub struct SqliteStoragePlugin;

impl Plugin for SqliteStoragePlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "storage-sqlite", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_storage("sqlite", Arc::new(SqliteStorage::default()))
    }
}

/// An open database.
struct Open {
    /// `None` when opened read-only.
    writer: Option<Mutex<Connection>>,
    reader: Mutex<Connection>,
    kinds: HashMap<String, KindSpec>,
    /// The run records are written to, or the last run when read-only.
    run: u32,
    /// Held while the database is open for writing, so a second GDS is refused (STO-10).
    _lock: Option<std::fs::File>,
}

#[derive(Default)]
pub struct SqliteStorage(Mutex<Option<Arc<Open>>>);

fn sql(e: rusqlite::Error) -> Error {
    Error::other(format!("sqlite: {e}"))
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The file of the database: the one given, or `gds.db` in the run's log directory (STO-3).
fn database_path(cfg: &StorageConfig) -> Result<PathBuf> {
    match (&cfg.database, &cfg.run_dir) {
        (Some(path), _) => Ok(path.clone()),
        (None, Some(dir)) => Ok(dir.join("gds.db")),
        (None, None) => Err(Error::Config("storage sqlite needs a database file or a log directory".into())),
    }
}

fn meta(conn: &Connection, key: &str) -> Result<Option<i64>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0)).optional().map_err(sql)
}

fn set_meta(conn: &Connection, key: &str, value: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
        params![key, value],
    )
    .map(|_| ())
    .map_err(sql)
}

fn check_version(conn: &Connection, path: &Path) -> Result<()> {
    match meta(conn, "schema_version")? {
        Some(version) if version > SCHEMA_VERSION => Err(Error::Config(format!(
            "{}: written by a newer GDS (schema {version}, this GDS knows {SCHEMA_VERSION})",
            path.display()
        ))),
        _ => Ok(()),
    }
}

fn run_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunInfo> {
    let plugins: String = row.get(5)?;
    Ok(RunInfo {
        run: row.get(0)?,
        started: Timestamp(row.get(1)?),
        gds_version: row.get(2)?,
        dictionary_version: row.get(3)?,
        dictionary_hash: row.get(4)?,
        plugins: serde_json::from_str(&plugins).unwrap_or_default(),
        first_seq: row.get::<_, i64>(6)? as u64,
        reserved_seq: 0,
    })
}

const RUN_COLUMNS: &str = "run, started, gds_version, dictionary_version, dictionary_hash, plugins, first_seq";

fn read_runs(conn: &Connection) -> Result<Vec<RunInfo>> {
    let reserved = meta(conn, "reserved_seq")?.unwrap_or(0) as u64;
    let mut statement = conn.prepare(&format!("SELECT {RUN_COLUMNS} FROM runs ORDER BY run")).map_err(sql)?;
    let runs = statement.query_map([], run_from).map_err(sql)?;
    let mut runs = runs.collect::<rusqlite::Result<Vec<_>>>().map_err(sql)?;
    // Only the last run can still be given numbers.
    if let Some(last) = runs.last_mut() {
        last.reserved_seq = reserved;
    }
    Ok(runs)
}

fn record_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    let flight_time = match row.get::<_, Option<u16>>(6)? {
        Some(base) => Some(FlightTime { base, context: row.get(7)?, seconds: row.get(8)?, microseconds: row.get(9)? }),
        None => None,
    };
    let meta: String = row.get(11)?;
    let meta: BTreeMap<String, Value> = serde_json::from_str(&meta).unwrap_or_default();
    let meta = meta.into_iter().filter_map(|(key, value)| match value {
        Value::String(text) => Some((key, MetaValue::Text(text))),
        other => other.as_u64().map(|n| (key, MetaValue::U64(n))),
    });
    let body: String = row.get(12)?;
    Ok(Record {
        seq: row.get::<_, i64>(0)? as u64,
        run: row.get(1)?,
        ground_time: Timestamp(row.get(2)?),
        kind: row.get(3)?,
        id: row.get(4)?,
        entity: row.get(5)?,
        flight_time,
        stored: true,
        raw: Bytes::from(row.get::<_, Vec<u8>>(10)?),
        meta: PacketMeta(meta.collect()),
        body: serde_json::from_str(&body).unwrap_or(Value::Null),
    })
}

fn meta_json(meta: &PacketMeta) -> String {
    let map: serde_json::Map<String, Value> = meta
        .0
        .iter()
        .map(|(key, value)| {
            let value = match value {
                MetaValue::U64(n) => Value::from(*n),
                MetaValue::Text(text) => Value::from(text.as_str()),
            };
            (key.clone(), value)
        })
        .collect();
    Value::Object(map).to_string()
}

impl Open {
    fn writable(cfg: &StorageConfig) -> Result<(Open, RunInfo)> {
        let path = database_path(cfg)?;
        let at = |e: &dyn std::fmt::Display| Error::Config(format!("{}: {e}", path.display()));
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let lock = std::fs::File::create(&lock_path).map_err(|e| at(&e))?;
        if lock.try_lock().is_err() {
            return Err(at(&"the database is in use by another GDS"));
        }
        let mut conn = Connection::open(&path).map_err(|e| at(&e))?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| at(&e))?;
        // With the write-ahead log, a commit survives a killed process; a power loss
        // may lose the last commits but leaves the database consistent.
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(|e| at(&e))?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| at(&e))?;
        let known = conn.prepare("SELECT 1 FROM meta").is_ok();
        if known {
            check_version(&conn, &path)?;
        }
        let tx = conn.transaction().map_err(sql)?;
        tx.execute_batch(SCHEMA).map_err(|e| at(&e))?;
        set_meta(&tx, "schema_version", SCHEMA_VERSION)?;
        // After everything reserved and everything stored: no number is used twice,
        // whatever happened to the run before (STO-14).
        let reserved = meta(&tx, "reserved_seq")?.unwrap_or(0);
        let stored: i64 = tx.query_row("SELECT ifnull(max(seq), 0) FROM records", [], |r| r.get(0)).map_err(sql)?;
        let first_seq = reserved.max(stored) as u64 + 1;
        let reserved_seq = first_seq - 1 + RESERVE_BLOCK;
        set_meta(&tx, "reserved_seq", reserved_seq as i64)?;
        let run: u32 = tx.query_row("SELECT ifnull(max(run), 0) + 1 FROM runs", [], |r| r.get(0)).map_err(sql)?;
        let info = RunInfo {
            run,
            started: Timestamp::now(),
            gds_version: cfg.gds_version.clone(),
            dictionary_version: cfg.dictionary_version.clone(),
            plugins: cfg.plugins.clone(),
            first_seq,
            reserved_seq,
            dictionary_hash: cfg.dictionary_hash.clone(),
        };
        tx.execute(
            "INSERT OR IGNORE INTO dictionaries (hash, json) VALUES (?1, ?2)",
            params![cfg.dictionary_hash, cfg.dictionary_json],
        )
        .map_err(sql)?;
        tx.execute(
            &format!("INSERT INTO runs ({RUN_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
            params![
                run,
                info.started.0,
                info.gds_version,
                info.dictionary_version,
                info.dictionary_hash,
                serde_json::to_string(&info.plugins).unwrap_or_default(),
                first_seq as i64,
            ],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        let reader = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| at(&e))?;
        tracing::info!(database = %path.display(), run, first_seq, "sqlite storage open");
        let open = Open {
            writer: Some(Mutex::new(conn)),
            reader: Mutex::new(reader),
            kinds: kinds(cfg),
            run,
            _lock: Some(lock),
        };
        Ok((open, info))
    }

    /// Opens the file as it is: no run begins and nothing is written (STO-12).
    fn read_only(cfg: &StorageConfig) -> Result<(Open, RunInfo)> {
        let path = database_path(cfg)?;
        let at = |e: &dyn std::fmt::Display| Error::Config(format!("{}: {e}", path.display()));
        if !path.is_file() {
            return Err(at(&"no such database"));
        }
        // A database that was closed properly is opened as immutable: not one file is
        // created beside it. After a killed GDS the write-ahead log still holds the last
        // records, and reading it needs SQLite's shared-memory file beside the database.
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let reader = match wal.metadata().is_ok_and(|m| m.len() > 0) {
            true => Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY),
            false => {
                let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
                let absolute = std::path::absolute(&path).map_err(|e| at(&e))?;
                let quoted = absolute.display().to_string().replace('%', "%25").replace('?', "%3f").replace('#', "%23");
                Connection::open_with_flags(format!("file:{quoted}?immutable=1"), flags)
            }
        }
        .map_err(|e| at(&e))?;
        reader.prepare("SELECT 1 FROM meta").map_err(|_| at(&"not a GDS database"))?;
        check_version(&reader, &path)?;
        let last = read_runs(&reader)?.pop().ok_or_else(|| at(&"the database holds no run"))?;
        let open = Open { writer: None, reader: Mutex::new(reader), kinds: kinds(cfg), run: last.run, _lock: None };
        Ok((open, last))
    }

    fn append(&self, batch: &[Record]) -> Result<()> {
        let writer = self.writer.as_ref().ok_or_else(|| Error::other("the database is open read-only"))?;
        let mut conn = locked(writer);
        let tx = conn.transaction().map_err(sql)?;
        {
            // A sequence number already stored is left as it is.
            let insert =
                format!("INSERT OR IGNORE INTO records ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)");
            let mut insert = tx.prepare_cached(&insert).map_err(sql)?;
            for record in batch {
                let time = record.flight_time;
                insert
                    .execute(params![
                        record.seq as i64,
                        record.run,
                        record.ground_time.0,
                        record.kind,
                        record.id,
                        record.entity,
                        time.map(|t| t.base),
                        time.map(|t| t.context),
                        time.map(|t| t.seconds),
                        time.map(|t| t.microseconds),
                        record.raw.as_ref(),
                        meta_json(&record.meta),
                        record.body.to_string(),
                    ])
                    .map_err(sql)?;
            }
        }
        tx.commit().map_err(sql)
    }

    fn reserve(&self, through: u64) -> Result<()> {
        let Some(writer) = &self.writer else { return Ok(()) };
        let conn = locked(writer);
        // Never below what is stored: a number is not given twice.
        let stored: i64 = conn.query_row("SELECT ifnull(max(seq), 0) FROM records", [], |r| r.get(0)).map_err(sql)?;
        set_meta(&conn, "reserved_seq", stored.max(through as i64))
    }

    /// The conditions SQL can decide: kind, run, sequence number, identifier, ground time.
    fn conditions(&self, q: &Query) -> (String, Vec<Sql>) {
        let mut clauses = vec!["1".to_string()];
        let mut values: Vec<Sql> = Vec::new();
        let mut list = |column: &str, items: Vec<Sql>, clauses: &mut Vec<String>| {
            if !items.is_empty() {
                clauses.push(format!("{column} IN ({})", vec!["?"; items.len()].join(", ")));
                values.extend(items);
            }
        };
        list("kind", q.kinds.iter().map(|k| Sql::Text(k.clone())).collect(), &mut clauses);
        list("id", q.ids.iter().map(|id| Sql::Integer(*id as i64)).collect(), &mut clauses);
        let mut one = |clause: &str, value: i64, clauses: &mut Vec<String>| {
            clauses.push(clause.to_string());
            values.push(Sql::Integer(value));
        };
        if !q.all_runs {
            one("run = ?", q.run.unwrap_or(self.run) as i64, &mut clauses);
        }
        // Sequence numbers are stored as signed 64-bit integers.
        let seq = |n: u64| n.min(i64::MAX as u64) as i64;
        if let Some(after) = q.after_seq {
            one("seq > ?", seq(after), &mut clauses);
        }
        if let Some(before) = q.before_seq {
            one("seq < ?", seq(before), &mut clauses);
        }
        if let Some(time) = q.time {
            if let Some(from) = time.from {
                one("ground_time >= ?", from.0, &mut clauses);
            }
            if let Some(to) = time.to {
                one("ground_time < ?", to.0, &mut clauses);
            }
        }
        (clauses.join(" AND "), values)
    }

    /// Matching rows in `seq` order. Field and text filters are decided by
    /// `Query::matches`, the same code every storage uses, on the rows SQL selected.
    fn select(&self, q: &Query, sql_text: &str, values: Vec<Sql>, at_most: usize) -> Result<Vec<Record>> {
        let conn = locked(&self.reader);
        let mut statement = conn.prepare_cached(sql_text).map_err(sql)?;
        let mut rows = statement.query(params_from_iter(values)).map_err(sql)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(sql)? {
            let record = record_from(row).map_err(sql)?;
            if q.matches(&record, self.kinds.get(&record.kind), self.run) {
                records.push(record);
                if records.len() >= at_most {
                    break;
                }
            }
        }
        Ok(records)
    }

    fn query(&self, q: &Query) -> Result<Page> {
        let limit = q.limit as usize;
        let (conditions, values) = self.conditions(q);
        let mut text = format!("SELECT {COLUMNS} FROM records WHERE {conditions} ORDER BY seq");
        if q.filters.is_empty() && q.text.is_none() {
            // One more than the page, to know whether there is a next page.
            text.push_str(&format!(" LIMIT {}", limit + 1));
        }
        let records = self.select(q, &text, values, limit + 1)?;
        Ok(page(records, limit))
    }

    fn latest(&self, q: &Query) -> Result<Page> {
        let limit = q.limit as usize;
        let (conditions, values) = self.conditions(q);
        let entity_kinds: Vec<&str> = self.kinds.values().filter(|k| k.has_entities).map(|k| k.id.as_str()).collect();
        if q.filters.is_empty() && q.text.is_none() && entity_kinds.iter().all(|k| !k.contains('\'')) {
            // The latest per entity for kinds with entities, per identifier for the others.
            let quoted: Vec<String> = entity_kinds.iter().map(|k| format!("'{k}'")).collect();
            let key = format!(
                "CASE WHEN kind IN ({}) THEN 'e' || ifnull(entity, '') ELSE 'i' || ifnull(id, '') END",
                if quoted.is_empty() { "NULL".to_string() } else { quoted.join(", ") }
            );
            let text = format!(
                "SELECT {COLUMNS} FROM records WHERE seq IN \
                 (SELECT max(seq) FROM records WHERE {conditions} GROUP BY kind, {key}) ORDER BY seq"
            );
            let unfiltered = Query { filters: Vec::new(), text: None, ..Query::default() };
            let unfiltered = Query { all_runs: true, ..unfiltered };
            return Ok(page(self.select(&unfiltered, &text, values, usize::MAX)?, limit));
        }
        // With field or text filters: the latest among the records that match them.
        let text = format!("SELECT {COLUMNS} FROM records WHERE {conditions} ORDER BY seq");
        let mut latest = BTreeMap::new();
        for record in self.select(q, &text, values, usize::MAX)? {
            let key = match self.kinds.get(&record.kind).is_some_and(|k| k.has_entities) {
                true => (record.kind.clone(), record.entity.clone(), None),
                false => (record.kind.clone(), None, record.id),
            };
            latest.insert(key, record);
        }
        let mut records: Vec<Record> = latest.into_values().collect();
        records.sort_by_key(|r| r.seq);
        Ok(page(records, limit))
    }
}

fn kinds(cfg: &StorageConfig) -> HashMap<String, KindSpec> {
    cfg.kinds.iter().map(|k| (k.id.clone(), k.clone())).collect()
}

fn page(mut records: Vec<Record>, limit: usize) -> Page {
    let more = records.len() > limit;
    records.truncate(limit);
    Page { next: records.last().filter(|_| more).map(|r| r.seq), records }
}

impl SqliteStorage {
    /// Run `work` on the open database, on a blocking thread.
    async fn with<T: Send + 'static>(&self, work: impl FnOnce(&Open) -> Result<T> + Send + 'static) -> Result<T> {
        let open = locked(&self.0).clone().ok_or_else(|| Error::other("storage is not open"))?;
        tokio::task::spawn_blocking(move || work(&open)).await.map_err(Error::other)?
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn open(&self, cfg: &StorageConfig) -> Result<RunInfo> {
        let cfg = cfg.clone();
        let opening = move || if cfg.read_only { Open::read_only(&cfg) } else { Open::writable(&cfg) };
        let (open, run) = tokio::task::spawn_blocking(opening).await.map_err(Error::other)??;
        *locked(&self.0) = Some(Arc::new(open));
        Ok(run)
    }

    async fn append(&self, batch: &[Record]) -> Result<()> {
        let batch = batch.to_vec();
        self.with(move |open| open.append(&batch)).await
    }

    async fn reserve(&self, through: u64) -> Result<()> {
        self.with(move |open| open.reserve(through)).await
    }

    async fn dictionary(&self, run: u32) -> Result<Option<Vec<u8>>> {
        self.with(move |open| {
            let text = "SELECT json FROM dictionaries WHERE hash = (SELECT dictionary_hash FROM runs WHERE run = ?1)";
            locked(&open.reader).query_row(text, [run], |row| row.get(0)).optional().map_err(sql)
        })
        .await
    }

    async fn query(&self, q: &Query) -> Result<Page> {
        let q = q.clone();
        self.with(move |open| open.query(&q)).await
    }

    async fn latest(&self, q: &Query) -> Result<Page> {
        let q = q.clone();
        self.with(move |open| open.latest(&q)).await
    }

    async fn runs(&self) -> Result<Vec<RunInfo>> {
        self.with(|open| read_runs(&locked(&open.reader))).await
    }

    async fn close(&self) -> Result<()> {
        let Some(open) = locked(&self.0).take() else { return Ok(()) };
        tokio::task::spawn_blocking(move || {
            // The reader goes first: the last connection to close is then the writer,
            // which folds the write-ahead log into the file and removes it, so the file
            // alone is the database.
            match Arc::try_unwrap(open) {
                Ok(Open { writer, reader, .. }) => {
                    drop(reader);
                    drop(writer);
                }
                // A query is still running: its connection closes when it ends.
                Err(open) => {
                    if let Some(writer) = &open.writer {
                        let _ = locked(writer).execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
                    }
                }
            }
        })
        .await
        .map_err(Error::other)
    }
}
