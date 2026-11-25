//! Decoder of a data product file: header, records by dictionary, the two checksums,
//! and compressed products. Same steps, in the same order, as `fprime-dp decode`, so
//! the same input gives the same document or the same error.

use crate::types::{DpDictionary, RecordDef};
use fgds_core::dictionary::{exact, Reader, Type};
use fgds_core::serde_json::{json, Value};

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
/// The decoded header and the fields the index shows.
#[derive(Debug, Clone)]
pub struct Header {
    pub json: Value,
    /// Its size in bytes.
    pub size: usize,
    pub id: u64,
    pub priority: u64,
    /// Time base, context, seconds, microseconds.
    pub time: [u64; 4],
    pub proc_types: u64,
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

/// One value of `ty`, with its 64-bit integers as numbers.
fn read(dict: &DpDictionary, ty: &Type, reader: &mut Reader) -> Result<Value, DpError> {
    dict.dict.decode(ty, reader).map(|value| exact(ty, value)).map_err(|e| unexpected(e.0))
}

pub fn read_header(dict: &DpDictionary, data: &[u8]) -> Result<Header, DpError> {
    let ty = dict.header_type().map_err(unexpected)?;
    let size = dict.header_size().map_err(unexpected)?;
    let bytes =
        data.get(..size).ok_or_else(|| unexpected(format!("the header is {size} bytes, the file {}", data.len())))?;
    let json = read(dict, ty, &mut Reader::new(bytes))?;
    let int = |value: &Value| value.as_u64().unwrap_or(0);
    let time = &json["Time"];
    Ok(Header {
        size,
        id: int(&json["Id"]),
        priority: int(&json["Priority"]),
        time: [int(&time["base"]), int(&time["context"]), int(&time["seconds"]), int(&time["microseconds"])],
        proc_types: int(&json["ProcTypes"]),
        dp_state: json["DpState"].as_str().unwrap_or_default().to_string(),
        data_size: int(&json["DataSize"]),
        checksum: int(&json["Checksum"]) as u32,
        computed_checksum: crc32(&bytes[..size - CHECKSUM_LEN]),
        json,
    })
}

struct Record<'a> {
    def: &'a RecordDef,
    value: Value,
}

impl Record<'_> {
    fn to_json(&self) -> Value {
        let def = self.def;
        json!({
            "Record": {
                "record_id": def.id,
                "record_name": def.name,
                "record_type_name": def.ty.name(),
                "is_array": def.is_array,
                "description": def.description,
            },
            "Data": self.value,
        })
    }
}

/// Arrays of zero-size elements are refused above this length (LNK-9): they cost
/// memory without consuming input.
const MAX_EMPTY_ELEMENTS: u64 = 65_536;

/// The records of `data`, which is nothing but records.
fn read_records<'a>(dict: &'a DpDictionary, data: &[u8]) -> Result<Vec<Record<'a>>, DpError> {
    let id_type =
        dict.dict.types.get("FwDpIdType").ok_or_else(|| unexpected("the dictionary has no type FwDpIdType".into()))?;
    let mut reader = Reader::new(data);
    let mut records = Vec::new();
    while reader.remaining() > 0 {
        let id = read(dict, id_type, &mut reader)?
            .as_u64()
            .ok_or_else(|| unexpected("FwDpIdType is not an unsigned integer".into()))?;
        let Some(def) = dict.records.get(&id) else {
            return Err(DpError::Product(format!("Record ID {id} not found in dictionary")));
        };
        let value = if def.is_array {
            let count = read(dict, &dict.size_type(), &mut reader)?.as_u64().unwrap_or(0);
            if count > MAX_EMPTY_ELEMENTS && dict.dict.max_size(&def.ty) == 0 {
                return Err(unexpected(format!(
                    "Array of {count} empty elements is over the limit of {MAX_EMPTY_ELEMENTS}"
                )));
            }
            let mut items = Vec::new();
            for index in 0..count {
                let item = read(dict, &def.ty, &mut reader);
                items.push(item.map_err(|e| unexpected(format!("{}[{index}]: {e}", def.name)))?);
            }
            Value::Array(items)
        } else {
            read(dict, &def.ty, &mut reader).map_err(|e| unexpected(format!("{}: {e}", def.name)))?
        };
        records.push(Record { def, value });
    }
    Ok(records)
}

/// A record of the compression processor: its name ends with `dpCompressProc`, one character, `CompressionRecord`.
fn is_compression_record(record: &Record) -> bool {
    let Some(prefix) = record.def.name.strip_suffix("CompressionRecord") else { return false };
    let mut chars = prefix.chars();
    chars.next_back().is_some() && chars.as_str().ends_with("dpCompressProc")
}

/// The data the compression records hold, decompressed; `None` when a record is not one of them.
fn decompress(dict: &DpDictionary, records: &[Record], max_bytes: u64) -> Result<Option<Vec<u8>>, DpError> {
    let mut out = Vec::new();
    let metadata = "Svc.CompressionMetadata";
    let metadata =
        dict.dict.types.get(metadata).ok_or_else(|| unexpected(format!("the dictionary has no type {metadata}")))?;
    for record in records {
        if !is_compression_record(record) {
            return Ok(None);
        }
        let items = record
            .value
            .as_array()
            .ok_or_else(|| unexpected(format!("{} is not an array of bytes", record.def.name)))?;
        let bytes: Option<Vec<u8>> =
            items.iter().map(|item| item.as_u64().and_then(|i| u8::try_from(i).ok())).collect();
        let bytes = bytes.ok_or_else(|| unexpected(format!("{} is not an array of bytes", record.def.name)))?;
        let mut reader = Reader::new(&bytes);
        let header = read(dict, metadata, &mut reader)?;
        let algorithm = header["algorithm"]
            .as_str()
            .ok_or_else(|| unexpected("the compression metadata names no algorithm".into()))?;
        let rest = &bytes[bytes.len() - reader.remaining()..];
        let room = usize::try_from(max_bytes).unwrap_or(usize::MAX).saturating_sub(out.len());
        match algorithm {
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
    /// Each `{"Record": the definition, "Data": the value}`.
    pub records: Vec<Value>,
}

impl Decoded {
    /// The document `fgds dp decode` writes.
    pub fn to_json(&self) -> Value {
        json!({"Header": self.header.json, "Records": self.records})
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

/// Decode a whole product: header, records, both checksums; compressed records are opened unless
/// `options.decompress` is off.
pub fn decode(dict: &DpDictionary, data: &[u8], options: DecodeOptions) -> Result<Decoded, DpError> {
    if data.len() as u64 > options.max_bytes {
        return Err(over_limit(options.max_bytes));
    }
    let header = read_header(dict, data)?;
    if let Some(error) = header.checksum_error() {
        return Err(error);
    }
    let rest = &data[header.size..];
    let size = usize::try_from(header.data_size).ok().filter(|size| rest.len() >= size.saturating_add(CHECKSUM_LEN));
    let Some(size) = size else {
        return Err(DpError::Product("End of file reached while processing data product".into()));
    };
    let (body, stored) = (&rest[..size], &rest[size..size + CHECKSUM_LEN]);
    let (stored, computed) = (u32::from_be_bytes(stored.try_into().expect("four bytes")), crc32(body));
    if stored != computed {
        return Err(crc_error("Data", stored, computed));
    }
    let mut records = read_records(dict, body)?;
    let inner;
    if options.decompress && records.first().is_some_and(is_compression_record) {
        if let Some(data) = decompress(dict, &records, options.max_bytes)? {
            inner = data;
            records = read_records(dict, &inner)?;
        }
    }
    Ok(Decoded { header, records: records.iter().map(Record::to_json).collect() })
}
