//! Decoder of a data product file: header, records by dictionary, the two checksums,
//! and compressed products. Same steps, in the same order, as `fprime-dp decode`, so
//! the same input gives the same document or the same error.

use crate::json::Json;
use crate::types::{plain, to_jsonable, DpDictionary, RecordDef, Ty, Val};
use std::sync::Arc;

/// Length of the CRC-32 that ends the header and the data.
pub const CHECKSUM_LEN: usize = 4;

/// Largest product decoded, and largest decompressed data, unless configured (LNK-9).
pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;

const CRC32: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

pub fn crc32(data: &[u8]) -> u32 {
    CRC32.checksum(data)
}

/// Why a product was not decoded. `fprime-dp` reports the two classes differently.
#[derive(Debug, Clone, PartialEq)]
pub enum DpError {
    /// A fault of the product itself (Python `DataProductError`): `Error: <text>`.
    Product(String),
    /// Any other failure (another Python exception): `Unexpected error: <text>`.
    Unexpected(String),
}

impl DpError {
    pub fn text(&self) -> &str {
        match self {
            DpError::Product(text) | DpError::Unexpected(text) => text,
        }
    }
}

impl std::fmt::Display for DpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

fn unexpected(text: String) -> DpError {
    DpError::Unexpected(text)
}

fn crc_error(section: &str, expected: u32, calculated: u32) -> DpError {
    DpError::Product(format!("CRC mismatch in {section}: expected {expected:#x}, got {calculated:#x}"))
}

/// A file read front to back, as the Python decoder reads its file handle.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    /// Up to `n` bytes: fewer at the end of the data, as `file.read(n)`.
    fn read(&mut self, n: u64) -> &'a [u8] {
        let end = self.data.len().min(self.at.saturating_add(usize::try_from(n).unwrap_or(usize::MAX)));
        let bytes = &self.data[self.at..end];
        self.at = end;
        bytes
    }
}

/// The decoded header, with the fields the index lists.
#[derive(Debug, Clone)]
pub struct Header {
    pub json: Json,
    pub size: usize,
    pub id: i128,
    pub priority: i128,
    /// Time base, context, seconds, microseconds.
    pub time: [i128; 4],
    pub proc_types: i128,
    pub dp_state: String,
    pub data_size: u64,
    pub checksum: u32,
    pub computed_checksum: u32,
}

impl Header {
    pub fn checksum_error(&self) -> Option<DpError> {
        (self.checksum != self.computed_checksum).then(|| crc_error("Header", self.checksum, self.computed_checksum))
    }
}

/// Read the header at the start of `data`. Its checksum is computed, not judged:
/// see `Header::checksum_error`.
pub fn read_header(dict: &DpDictionary, data: &[u8]) -> Result<Header, DpError> {
    let ty = Ty::Struct(dict.header_type().map_err(unexpected)?);
    let size = dict.max_size(&ty).map_err(unexpected)?;
    let bytes = Cursor { data, at: 0 }.read(size);
    let (value, _) = dict.deserialize(&ty, bytes, 0).map_err(unexpected)?;
    let json = to_jsonable(&ty, &value);
    let Val::Struct(fields) = &value else { unreachable!("the header is a struct") };
    let int = |index: usize| match &fields[index] {
        Val::Int(i) => Ok(*i),
        _ => Err(unexpected(format!("header field {index} is not an integer"))),
    };
    let time = match &fields[3] {
        Val::Time(parts) => {
            let part = |v: &Val| match v {
                Val::Int(i) | Val::Enum(_, i) => *i,
                _ => 0,
            };
            [part(&parts[0]), part(&parts[1]), part(&parts[2]), part(&parts[3])]
        }
        _ => [0; 4],
    };
    let dp_state = match plain(&Ty::Bool, &fields[6]) {
        Json::Str(name) => name,
        other => other.to_python_text(),
    };
    // The checksum covers the header up to the checksum itself.
    let computed_checksum = crc32(&bytes[..bytes.len().saturating_sub(CHECKSUM_LEN)]);
    Ok(Header {
        json,
        size: bytes.len(),
        id: int(1)?,
        priority: int(2)?,
        time,
        proc_types: int(4)?,
        dp_state,
        data_size: u64::try_from(int(7)?).map_err(|_| unexpected("negative data size".into()))?,
        checksum: int(8)? as u32,
        computed_checksum,
    })
}

struct Record {
    def: Arc<RecordView>,
    ty: Ty,
    value: Val,
}

/// What the output shows of a record definition.
struct RecordView {
    name: String,
    json: Json,
}

fn record_view(def: &RecordDef) -> RecordView {
    let entries = [
        ("record_id", Json::Int(def.id)),
        ("record_name", Json::str(&def.name)),
        ("record_type_name", Json::str(def.ty.class_name())),
        ("record_type", Json::str(def.ty.class_text())),
        ("is_array", Json::Bool(def.is_array)),
        ("description", def.description.as_ref().map_or(Json::Null, Json::str)),
    ];
    RecordView {
        name: def.name.clone(),
        json: Json::Map(entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect()),
    }
}

impl Record {
    fn to_json(&self) -> Json {
        Json::Map(vec![("Record".into(), self.def.json.clone()), ("Data".into(), to_jsonable(&self.ty, &self.value))])
    }
}

/// One element at the cursor: at most its largest size is looked at, and the cursor
/// moves by the size it really has.
fn read_element(dict: &DpDictionary, cursor: &mut Cursor, ty: &Ty) -> Result<Val, DpError> {
    let start = cursor.at;
    let buffer = cursor.read(dict.max_size(ty).map_err(unexpected)?);
    let (value, size) = dict.deserialize(ty, buffer, 0).map_err(unexpected)?;
    cursor.at = start + size;
    Ok(value)
}

/// Records until `data_size` bytes are consumed.
fn read_records(dict: &DpDictionary, cursor: &mut Cursor, data_size: u64) -> Result<Vec<Record>, DpError> {
    let id_type = dict.get_type("FwDpIdType").map_err(unexpected)?;
    let id_size = dict.max_size(&id_type).map_err(unexpected)?;
    let start = cursor.at;
    let mut views: std::collections::HashMap<i128, Arc<RecordView>> = Default::default();
    let mut records = Vec::new();
    while ((cursor.at - start) as u64) < data_size {
        let id_bytes = cursor.read(id_size);
        if id_bytes.is_empty() {
            return Err(DpError::Product("End of file reached while processing data product".into()));
        }
        let (id, _) = dict.deserialize(&id_type, id_bytes, 0).map_err(unexpected)?;
        let id = id.int().ok_or_else(|| unexpected("FwDpIdType is not an integer".into()))?;
        let Some(def) = dict.records.get(&id) else {
            return Err(DpError::Product(format!("Record ID {id} not found in dictionary")));
        };
        let ty = if def.is_array {
            // An array record: the number of elements, then the elements.
            let count_type = dict.get_type("FwSizeStoreType").map_err(unexpected)?;
            let count_bytes = cursor.read(dict.max_size(&count_type).map_err(unexpected)?);
            let (count, _) = dict.deserialize(&count_type, count_bytes, 0).map_err(unexpected)?;
            let len = count.int().and_then(|n| u64::try_from(n).ok());
            let len = len.ok_or_else(|| unexpected("FwSizeStoreType is not an unsigned integer".into()))?;
            let name = format!("{}_{len}", def.name);
            Ty::Array(Arc::new(crate::types::ArrayTy { name, len, format: "{}".into(), element: def.ty.clone() }))
        } else {
            def.ty.clone()
        };
        let value = read_element(dict, cursor, &ty)?;
        let def = views.entry(id).or_insert_with(|| Arc::new(record_view(def))).clone();
        records.push(Record { def, ty, value });
    }
    Ok(records)
}

/// A record written by the compression processor: `...dpCompressProc<any character>CompressionRecord`.
fn is_compression_record(record: &Record) -> bool {
    let Some(prefix) = record.def.name.strip_suffix("CompressionRecord") else { return false };
    let mut chars = prefix.chars();
    chars.next_back().is_some_and(|separator| separator != '\n') && chars.as_str().ends_with("dpCompressProc")
}

/// The data inside compression records; `None` when a record is not one of them.
fn decompress(dict: &DpDictionary, records: &[Record], max_bytes: u64) -> Result<Option<Vec<u8>>, DpError> {
    let mut out = Vec::new();
    let metadata = dict.get_type("Svc.CompressionMetadata").map_err(unexpected)?;
    for record in records {
        if !is_compression_record(record) {
            return Ok(None);
        }
        let Val::List(items) = &record.value else { return Err(unexpected("'values'".into())) };
        let bytes: Option<Vec<u8>> = items.iter().map(|item| item.int().and_then(|i| u8::try_from(i).ok())).collect();
        let bytes = bytes.ok_or_else(|| unexpected("bytes must be in range(0, 256)".into()))?;
        let mut cursor = Cursor { data: &bytes, at: 0 };
        let header = cursor.read(dict.max_size(&metadata).map_err(unexpected)?);
        let (value, _) = dict.deserialize(&metadata, header, 0).map_err(unexpected)?;
        let algorithm = match plain(&metadata, &value).get("algorithm") {
            Some(Json::Str(name)) => name.clone(),
            Some(other) => other.to_python_text(),
            None => return Err(unexpected("'algorithm'".into())),
        };
        let rest = &bytes[cursor.at..];
        let room = usize::try_from(max_bytes).unwrap_or(usize::MAX).saturating_sub(out.len());
        match algorithm.as_str() {
            "UNCOMPRESSED" if rest.len() <= room => out.extend_from_slice(rest),
            "ZLIB_DEFLATE" => match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(rest, room) {
                Ok(data) => out.extend(data),
                Err(e) if e.status == miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
                    return Err(over_limit(max_bytes))
                }
                Err(e) => return Err(unexpected(format!("Error while decompressing data: {:?}", e.status))),
            },
            "UNCOMPRESSED" => return Err(over_limit(max_bytes)),
            other => return Err(DpError::Product(format!("Compression algorithm {other} unsupported"))),
        }
    }
    Ok(Some(out))
}

pub fn over_limit(max_bytes: u64) -> DpError {
    DpError::Product(format!("Data product is over the size limit of {max_bytes} bytes"))
}

/// A decoded product: the document `fprime-dp decode` writes.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub header: Header,
    pub records: Vec<Json>,
}

impl Decoded {
    pub fn to_json(&self) -> Json {
        Json::Map(vec![
            ("Header".into(), self.header.json.clone()),
            ("Records".into(), Json::List(self.records.clone())),
        ])
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DecodeOptions {
    /// Decode the records inside compression records (off with `-z`).
    pub decompress: bool,
    pub max_bytes: u64,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { decompress: true, max_bytes: DEFAULT_MAX_BYTES }
    }
}

/// Decode a whole product file. Never panics, whatever `data` holds (MNT-6).
pub fn decode(dict: &DpDictionary, data: &[u8], options: DecodeOptions) -> Result<Decoded, DpError> {
    if data.len() as u64 > options.max_bytes {
        return Err(over_limit(options.max_bytes));
    }
    let header = read_header(dict, data)?;
    if let Some(error) = header.checksum_error() {
        return Err(error);
    }
    let mut cursor = Cursor { data, at: header.size };
    let start = cursor.at;
    let mut records = read_records(dict, &mut cursor, header.data_size)?;
    // Python asserts this; the message of its AssertionError is empty.
    if (cursor.at - start) as u64 != header.data_size {
        return Err(unexpected(String::new()));
    }
    let checksum_type = Ty::Int { bits: 32, signed: false };
    let (stored, _) = dict.deserialize(&checksum_type, cursor.read(CHECKSUM_LEN as u64), 0).map_err(unexpected)?;
    let computed = crc32(&data[start..cursor.at - CHECKSUM_LEN]);
    if stored != Val::Int(i128::from(computed)) {
        return Err(crc_error("Data", stored.int().unwrap_or(0) as u32, computed));
    }
    if options.decompress && records.first().is_some_and(is_compression_record) {
        if let Some(inner) = decompress(dict, &records, options.max_bytes)? {
            records = read_records(dict, &mut Cursor { data: &inner, at: 0 }, inner.len() as u64)?;
        }
    }
    Ok(Decoded { header, records: records.iter().map(Record::to_json).collect() })
}
