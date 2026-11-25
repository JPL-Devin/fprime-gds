//! The index of data products (DP-1, DP-5..8): files of the watched directory become
//! records of kind `fprime.data_product`, one entity per content hash, and are read
//! again and decoded on demand by the routes.

use crate::decode::{decode, read_header, DecodeOptions, DpError, DEFAULT_MAX_BYTES};
use crate::types::DpDictionary;
use crate::validate::{validate, HeaderSize, Invalid};
use crate::{OPTION_DIR, OPTION_MAX_BYTES, OPTION_SETTLE_MS};
use fgds_core::axum::extract::{Path as UrlPath, Query as UrlQuery, State};
use fgds_core::axum::http::{header, StatusCode};
use fgds_core::axum::response::{IntoResponse, Response};
use fgds_core::axum::routing::get;
use fgds_core::axum::{Json, Router};
use fgds_core::serde_json::{json, Value};
use fgds_core::{
    async_trait, Access, ApiError, Context, FieldFilter, FieldSpec, FieldType, Filter, FlightTime, Handler, KindSpec,
    NewRecord, Op, PluginConfig, Query, RecordStream, Result, RouteAccess, Routes,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub const KIND: &str = "fprime.data_product";

/// The only source until the files plugin reports completed downlinks (DP-1).
const SOURCE_WATCH: &str = "watch";

/// Sub-directories of the watched directory are followed this deep.
const MAX_DEPTH: usize = 8;

const SCHEMA: &str = r#"{
  "type": "object",
  "required": ["hash", "name", "sources", "missing", "size", "valid", "error"],
  "properties": {
    "hash": {"type": "string", "description": "SHA-256 of the file content, lowercase hex; also the entity"},
    "name": {"type": "string", "description": "File name of the first source"},
    "sources": {"type": "array", "items": {"type": "object", "properties": {
      "source": {"type": "string", "description": "watch"},
      "path": {"type": "string", "description": "Relative to the watched directory"}}}},
    "missing": {"type": "boolean", "description": "No file with this content is left"},
    "size": {"type": "integer"},
    "container": {"type": ["string", "null"]},
    "container_id": {"type": ["integer", "null"]},
    "priority": {"type": ["integer", "null"]},
    "time": {"type": ["object", "null"], "description": "base, context, seconds, microseconds"},
    "proc_types": {"type": ["integer", "null"]},
    "dp_state": {"type": ["string", "null"]},
    "data_size": {"type": ["integer", "null"]},
    "records": {"type": ["integer", "null"], "description": "Number of decoded records"},
    "valid": {"type": "boolean"},
    "error": {"type": ["string", "null"], "description": "First validation failure"}
  }
}"#;

pub(crate) fn kind() -> KindSpec {
    KindSpec {
        schema: SCHEMA,
        has_entities: true,
        fields: &[
            FieldSpec { name: "hash", pointer: "/hash", ty: FieldType::Text },
            FieldSpec { name: "name", pointer: "/name", ty: FieldType::Text },
            FieldSpec { name: "container", pointer: "/container", ty: FieldType::Text },
            FieldSpec { name: "priority", pointer: "/priority", ty: FieldType::Number },
            FieldSpec { name: "size", pointer: "/size", ty: FieldType::Number },
            FieldSpec { name: "valid", pointer: "/valid", ty: FieldType::Bool },
            FieldSpec { name: "missing", pointer: "/missing", ty: FieldType::Bool },
        ],
        text_fields: &["/name", "/container", "/error"],
        ..KindSpec::new(KIND, "A data product file; entity is its content hash, id its container identifier")
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The watched directory; nothing is indexed without one.
    pub dir: Option<PathBuf>,
    /// A file is indexed once its size has not changed for this long (DP-6).
    pub settle: Duration,
    pub max_bytes: u64,
}

impl Options {
    pub fn from_config(config: &PluginConfig) -> Options {
        let number = |key: &str, default: u64| config.get(key).and_then(|v| v.parse().ok()).unwrap_or(default);
        Options {
            dir: config.get(OPTION_DIR).filter(|dir| !dir.is_empty()).map(PathBuf::from),
            settle: Duration::from_millis(number(OPTION_SETTLE_MS, 2000)),
            max_bytes: number(OPTION_MAX_BYTES, DEFAULT_MAX_BYTES),
        }
    }
}

/// SHA-256 of a file, read in pieces so a file over the size limit is never held in memory.
fn hash_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let (mut hasher, mut size, mut buffer) = (Sha256::new(), 0u64, vec![0u8; 64 * 1024]);
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            let hex = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
            return Ok((hex, size));
        }
        hasher.update(&buffer[..n]);
        size += n as u64;
    }
}

/// What the index keeps of a product: header fields and the first validation failure.
#[derive(Debug, Clone, PartialEq)]
struct Summary {
    size: u64,
    fields: Value,
    flight_time: Option<FlightTime>,
    container_id: Option<u32>,
    error: Option<String>,
}

/// Validate a product (DP-5): header, container, checksums, then every record.
fn summarize(dict: &DpDictionary, path: &Path, size: u64, max_bytes: u64) -> Summary {
    let mut summary = Summary { size, fields: json!({}), flight_time: None, container_id: None, error: None };
    if size > max_bytes {
        summary.error = Some(crate::decode::over_limit(max_bytes).to_string());
        return summary;
    }
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) => {
            summary.error = Some(format!("Unreadable: {e}"));
            return summary;
        }
    };
    let header = match read_header(dict, &data) {
        Ok(header) => header,
        Err(e) => {
            summary.error = Some(format!("Truncated or unreadable header: {e}"));
            return summary;
        }
    };
    let container = dict.containers.get(&header.id);
    let [base, context, seconds, microseconds] = header.time;
    summary.fields = json!({
        "container": container.map(|c| c.name.clone()),
        "container_id": header.id,
        "priority": header.priority,
        "time": {"base": base, "context": context, "seconds": seconds, "microseconds": microseconds},
        "proc_types": header.proc_types,
        "dp_state": header.dp_state,
        "data_size": header.data_size,
    });
    summary.container_id = u32::try_from(header.id).ok();
    summary.flight_time = Some(FlightTime {
        base: base as u16,
        context: context as u8,
        seconds: seconds as u32,
        microseconds: microseconds as u32,
    });
    let checksums = validate(&data, HeaderSize::Dictionary(header.size)).result;
    let decoded = decode(dict, &data, DecodeOptions { decompress: true, max_bytes });
    summary.fields["records"] = json!(decoded.as_ref().ok().map(|d| d.records.len()));
    summary.error = match (header.checksum_error(), container, checksums, decoded) {
        (Some(e), ..) => Some(e.to_string()),
        (None, None, ..) => Some(format!("Unknown container: id {} is not in the dictionary", header.id)),
        (None, Some(_), Err(Invalid::Failed(e) | Invalid::Crash(e)), _) => Some(e),
        (None, Some(_), Ok(()), Err(e)) => Some(e.to_string()),
        (None, Some(_), Ok(()), Ok(_)) => None,
    };
    summary
}

/// What was last seen of one file of the watched directory.
struct Seen {
    size: u64,
    modified: Option<SystemTime>,
    /// When this size was first seen.
    since: Instant,
    /// Size, modification time and hash when it was indexed.
    indexed: Option<(u64, Option<SystemTime>, String)>,
}

struct Product {
    summary: Summary,
    /// Paths relative to the watched directory that hold this content now.
    paths: BTreeSet<String>,
    /// The name shown when no path is left.
    name: String,
    published: Option<Value>,
}

/// The state of the watched directory. `scan` is called at intervals and returns the
/// records to publish; it holds no file content.
pub struct Index {
    options: Options,
    files: HashMap<String, Seen>,
    products: BTreeMap<String, Product>,
}

impl Index {
    pub fn new(options: Options) -> Index {
        Index { options, files: HashMap::new(), products: BTreeMap::new() }
    }

    /// Regular files named `*.fdp` under `dir`; symbolic links are not followed (DP-6).
    fn list(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(String, std::fs::Metadata)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let (Ok(name), Ok(meta)) = (entry.file_name().into_string(), std::fs::symlink_metadata(entry.path()))
            else {
                continue;
            };
            let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if meta.is_dir() && depth < MAX_DEPTH {
                Index::list(&entry.path(), &relative, depth + 1, out);
            } else if meta.is_file() && name.ends_with(".fdp") {
                out.push((relative, meta));
            }
        }
    }

    /// Look at the directory once; `now` is the time of the look.
    pub fn scan(&mut self, dict: &DpDictionary, now: Instant) -> Vec<NewRecord> {
        let Some(dir) = self.options.dir.clone() else { return Vec::new() };
        let mut listed = Vec::new();
        Index::list(&dir, "", 0, &mut listed);
        let present: BTreeSet<&String> = listed.iter().map(|(path, _)| path).collect();
        let gone: Vec<String> = self.files.keys().filter(|path| !present.contains(path)).cloned().collect();
        for path in gone {
            if let Some((_, _, hash)) = self.files.remove(&path).and_then(|seen| seen.indexed) {
                self.forget(&hash, &path);
            }
        }
        for (path, meta) in &listed {
            let (size, modified) = (meta.len(), meta.modified().ok());
            let seen = self.files.entry(path.clone()).or_insert(Seen { size, modified, since: now, indexed: None });
            if seen.size != size {
                (seen.size, seen.since) = (size, now);
            }
            seen.modified = modified;
            let current = seen.indexed.as_ref().is_some_and(|(s, m, _)| (*s, *m) == (size, modified));
            if current || now.duration_since(seen.since) < self.options.settle {
                continue;
            }
            // The file may be replaced while it is read: only its hash names what was read.
            let Ok((hash, hashed_size)) = hash_file(&dir.join(path)) else { continue };
            let previous = seen.indexed.replace((size, modified, hash.clone()));
            if let Some(product) = previous.and_then(|(_, _, old)| self.products.get_mut(&old)) {
                product.paths.remove(path);
            }
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            let max_bytes = self.options.max_bytes;
            let product = self.products.entry(hash).or_insert_with(|| Product {
                summary: summarize(dict, &dir.join(path), hashed_size, max_bytes),
                paths: BTreeSet::new(),
                name: name.clone(),
                published: None,
            });
            product.paths.insert(path.clone());
        }
        let mut records = Vec::new();
        for (hash, product) in &mut self.products {
            if let Some(name) = product.paths.first().and_then(|path| path.rsplit('/').next()) {
                product.name = name.to_string();
            }
            let mut body = json!({
                "hash": hash,
                "name": product.name,
                "sources": product.paths.iter().map(|path| json!({"source": SOURCE_WATCH, "path": path})).collect::<Vec<_>>(),
                "missing": product.paths.is_empty(),
                "size": product.summary.size,
                "container": null, "container_id": null, "priority": null, "time": null, "proc_types": null,
                "dp_state": null, "data_size": null, "records": null,
                "valid": product.summary.error.is_none(),
                "error": product.summary.error,
            });
            for (key, value) in product.summary.fields.as_object().into_iter().flatten() {
                body[key] = value.clone();
            }
            if product.published.as_ref() != Some(&body) {
                product.published = Some(body.clone());
                records.push(NewRecord {
                    entity: Some(hash.clone()),
                    id: product.summary.container_id,
                    flight_time: product.summary.flight_time,
                    ..NewRecord::new(KIND, body)
                });
            }
        }
        records
    }

    fn forget(&mut self, hash: &str, path: &str) {
        if let Some(product) = self.products.get_mut(hash) {
            product.paths.remove(path);
        }
    }
}

/// The handler that keeps the index up to date while the GDS runs.
pub(crate) struct Indexer {
    options: Options,
}

impl Indexer {
    pub(crate) fn new(options: Options) -> Indexer {
        Indexer { options }
    }
}

#[async_trait]
impl Handler for Indexer {
    fn subscription(&self) -> Filter {
        Filter::kinds(&[KIND])
    }

    async fn run(self: Box<Self>, mut ctx: Context, mut records: RecordStream) -> Result<()> {
        let Some(dir) = self.options.dir.clone() else {
            ctx.status.set_state("no watched directory");
            return Ok(());
        };
        let dict = DpDictionary::from_json(&ctx.dictionary.json).map_err(fgds_core::Error::other)?;
        let dict = std::sync::Arc::new(dict);
        ctx.status.set_state(&format!("watching {}", dir.display()));
        let interval = (self.options.settle / 4).clamp(Duration::from_millis(50), Duration::from_secs(1));
        let mut index = Index::new(self.options);
        loop {
            let scan_dict = dict.clone();
            // File reads and decoding stay off the runtime threads.
            let scanned = tokio::task::spawn_blocking(move || {
                let found = index.scan(&scan_dict, Instant::now());
                (index, found)
            });
            let found;
            (index, found) = scanned.await.map_err(fgds_core::Error::other)?;
            ctx.status.count("products_indexed", found.len() as u64);
            for record in found {
                ctx.records.publish(record).await?;
            }
            let wait = tokio::time::sleep(interval);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = ctx.shutdown.cancelled() => return Ok(()),
                    _ = &mut wait => break,
                    // Its own records come back on the subscription; nothing to do with them.
                    record = records.next() => if record.is_none() { return Ok(()) },
                }
            }
        }
    }
}

pub(crate) fn routes() -> Routes {
    let read = |path| RouteAccess { method: "GET", path, access: Access::Read };
    Routes {
        prefix: "data-products",
        router: Router::new().route("/", get(list)).route("/:hash", get(detail)).route("/:hash/export", get(export)),
        access: vec![read("/"), read("/:hash"), read("/:hash/export")],
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// `GET /api/v1/data-products`: every product of this run, as last indexed.
async fn list(State(ctx): State<Context>) -> ApiResult<Json<Value>> {
    let page = ctx.records.latest(Query { kinds: vec![KIND.into()], limit: 10_000, ..Default::default() }).await?;
    let products: Vec<&Value> = page.records.iter().map(|record| &record.body).collect();
    let directory = Options::from_config(&ctx.config).dir.map(|dir| dir.display().to_string());
    Ok(Json(json!({"directory": directory, "products": products})))
}

fn missing(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "missing", message)
}

/// The index entry of `hash`.
async fn entry(ctx: &Context, hash: &str) -> ApiResult<Value> {
    let by_hash = FieldFilter { field: "hash".into(), op: Op::Eq, value: json!(hash) };
    let query = Query { kinds: vec![KIND.into()], filters: vec![by_hash], limit: 1, ..Default::default() };
    let page = ctx.records.latest(query).await?;
    let record = page.records.into_iter().next();
    record
        .map(|record| record.body)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_product", format!("no data product {hash}")))
}

/// The content of an indexed product, read again from the watched directory. A file
/// that is gone, changed, or outside the directory is missing and is not read (DP-8).
fn content(options: &Options, body: &Value) -> ApiResult<Vec<u8>> {
    let hash = body["hash"].as_str().unwrap_or_default();
    let dir = options.dir.as_ref().ok_or_else(|| missing("no watched directory is configured for this run"))?;
    let root = dir.canonicalize().map_err(|_| missing("the watched directory is gone"))?;
    for source in body["sources"].as_array().into_iter().flatten() {
        let Some(relative) = source["path"].as_str().map(Path::new) else { continue };
        if !relative.components().all(|part| matches!(part, Component::Normal(_))) {
            continue;
        }
        let path = dir.join(relative);
        let regular =
            std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() <= options.max_bytes);
        let inside = path.canonicalize().is_ok_and(|real| real.starts_with(&root));
        if !regular || !inside {
            continue;
        }
        match std::fs::read(&path) {
            Ok(data) if format!("{:x}", Sha256::digest(&data)) == hash => return Ok(data),
            _ => continue,
        }
    }
    Err(missing("the file of this data product is missing"))
}

/// `?decompress=false` keeps compression records as they are, as `fprime-dp decode -z`.
fn options_of(ctx: &Context, query: &HashMap<String, String>) -> (Options, DecodeOptions) {
    let options = Options::from_config(&ctx.config);
    let decompress = query.get("decompress").is_none_or(|value| value != "false");
    let decode_options = DecodeOptions { decompress, max_bytes: options.max_bytes };
    (options, decode_options)
}

fn decoded(
    ctx: &Context,
    body: &Value,
    query: &HashMap<String, String>,
) -> ApiResult<std::result::Result<crate::Decoded, DpError>> {
    let (options, decode_options) = options_of(ctx, query);
    let data = content(&options, body)?;
    let dict = DpDictionary::from_json(&ctx.dictionary.json).map_err(fgds_core::Error::other)?;
    Ok(decode(&dict, &data, decode_options))
}

/// `GET /api/v1/data-products/<hash>`: the index entry with its decoded header and
/// records, or the reason it does not decode. Values are encoded per API-4.
async fn detail(
    State(ctx): State<Context>,
    UrlPath(hash): UrlPath<String>,
    UrlQuery(query): UrlQuery<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let body = entry(&ctx, &hash).await?;
    let (decoded, error) = match decoded(&ctx, &body, &query) {
        Ok(Ok(decoded)) => (crate::json::for_api(&decoded.to_json()), Value::Null),
        Ok(Err(e)) => (Value::Null, json!({"code": "decode_failed", "message": e.to_string()})),
        Err(e) if e.code == "missing" => (Value::Null, json!({"code": e.code, "message": e.message})),
        Err(e) => return Err(e),
    };
    Ok(Json(json!({"product": body, "decoded": decoded, "error": error})))
}

/// `GET /api/v1/data-products/<hash>/export`: the JSON file `fgds dp decode` writes
/// for this product, byte for byte (DP-4).
async fn export(
    State(ctx): State<Context>,
    UrlPath(hash): UrlPath<String>,
    UrlQuery(query): UrlQuery<HashMap<String, String>>,
) -> ApiResult<Response> {
    let body = entry(&ctx, &hash).await?;
    let decoded = decoded(&ctx, &body, &query)?
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "decode_failed", e.to_string()))?;
    let name = Path::new(body["name"].as_str().unwrap_or("product")).with_extension("json");
    // The name comes from a file: keep only what is safe inside a quoted header value.
    let name: String =
        name.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c)).collect();
    let headers = [
        (header::CONTENT_TYPE, "application/json".to_string()),
        (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
    ];
    Ok((headers, fgds_core::serde_json::to_string_pretty(&decoded.to_json()).unwrap_or_default()).into_response())
}
