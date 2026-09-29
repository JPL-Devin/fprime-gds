//! Data product decoder, mirroring `fprime_gds.common.dp.decoder`.

use flate2::read::ZlibDecoder;
use serde_json::{json, Map, Value};
use std::io::Read;

use crate::dictionary::Dictionary;
use crate::types::{take, FType, IntKind};

pub const CHECKSUM_LEN: usize = 4;

pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Header field descriptions from `common.get_dp_header_type()`
const HEADER_DESCRIPTIONS: [(&str, &str); 9] = [
    ("PacketDescriptor", "The F Prime packet descriptor"),
    ("Id", "The container ID"),
    ("Priority", "The container priority"),
    ("Time", "Fw.Time object"),
    ("ProcTypes", "Processing types bit mask"),
    ("UserData", "User-configurable data"),
    ("DpState", "Data product state"),
    ("DataSize", "Size of data payload in bytes"),
    ("Checksum", "Header checksum"),
];

pub struct Decoder<'a> {
    dictionary: &'a Dictionary,
    size_store_len: usize,
    dp_id_type: FType,
    header_type: FType,
}

impl<'a> Decoder<'a> {
    pub fn new(dictionary: &'a Dictionary) -> Result<Decoder<'a>, String> {
        let size_store_len = dictionary.resolve("FwSizeStoreType")?.max_size(0);
        let proc_type = dictionary.resolve("Fw.DpCfg.ProcType")?;
        let proc_rep = match &proc_type {
            FType::Enum(_, rep, _) => FType::Int(*rep),
            other => other.clone(),
        };
        let user_data_size = dictionary.get_constant("Fw.DpCfg.CONTAINER_USER_DATA_SIZE")?;
        let members: Vec<(&str, FType)> = vec![
            (
                "PacketDescriptor",
                dictionary.resolve("FwPacketDescriptorType")?,
            ),
            ("Id", dictionary.resolve("FwDpIdType")?),
            ("Priority", dictionary.resolve("FwDpPriorityType")?),
            ("Time", FType::Time),
            ("ProcTypes", proc_rep),
            (
                "UserData",
                FType::Array(
                    "UserData".to_string(),
                    Box::new(FType::Int(IntKind::U8)),
                    user_data_size as usize,
                    "{}".to_string(),
                ),
            ),
            ("DpState", dictionary.resolve("Fw.DpState")?),
            ("DataSize", dictionary.resolve("FwSizeStoreType")?),
            ("Checksum", FType::Int(IntKind::U32)),
        ];
        let header_type = FType::Struct(
            "DataProductHeaderType".to_string(),
            members
                .into_iter()
                .zip(HEADER_DESCRIPTIONS.iter())
                .map(|((name, ftype), (_, desc))| {
                    (name.to_string(), ftype, "{}".to_string(), desc.to_string())
                })
                .collect(),
        );
        Ok(Decoder {
            dictionary,
            size_store_len,
            dp_id_type: dictionary.resolve("FwDpIdType")?,
            header_type,
        })
    }

    /// Size in bytes of the data product header for this dictionary
    pub fn header_size(&self) -> usize {
        self.header_type.max_size(self.size_store_len)
    }

    /// Decode the header, validating its checksum
    fn decode_header(&self, buf: &[u8], offset: &mut usize) -> Result<Value, String> {
        let header_size = self.header_type.max_size(self.size_store_len);
        let start = *offset;
        let header_val = self
            .header_type
            .deserialize(buf, offset, self.size_store_len)?;
        let computed = crc32(&buf[start..start + header_size - CHECKSUM_LEN]);
        let stored = header_val["Checksum"].as_u64().unwrap_or(0) as u32;
        if stored != computed {
            return Err(format!(
                "CRC mismatch in Header: expected {:#x}, got {:#x}",
                stored, computed
            ));
        }
        Ok(self.header_type.jsonable(&header_val))
    }

    /// Decode a single record; buf positioned at the record ID
    fn decode_record(&self, buf: &[u8], offset: &mut usize) -> Result<Value, String> {
        let id_val = self
            .dp_id_type
            .deserialize(buf, offset, self.size_store_len)
            .map_err(|_| "End of file reached while processing data product".to_string())?;
        let record_id = id_val.as_u64().unwrap_or(0);
        let record = self
            .dictionary
            .records
            .get(&record_id)
            .ok_or_else(|| format!("Record ID {} not found in dictionary", record_id))?;

        let mut record_meta = Map::new();
        record_meta.insert("record_id".to_string(), json!(record.id));
        record_meta.insert("record_name".to_string(), json!(record.name));
        record_meta.insert(
            "record_type_name".to_string(),
            json!(record.ftype.class_name()),
        );
        record_meta.insert("record_type".to_string(), json!(record.ftype.class_str()));
        record_meta.insert("is_array".to_string(), json!(record.is_array));
        record_meta.insert("description".to_string(), json!(record.description));

        let data = if record.is_array {
            let mut size: usize = 0;
            for b in take(buf, offset, self.size_store_len)? {
                size = (size << 8) | (*b as usize);
            }
            let array_type = FType::Array(
                format!("{}_{}", record.name, size),
                Box::new(record.ftype.clone()),
                size,
                "{}".to_string(),
            );
            let val = array_type.deserialize(buf, offset, self.size_store_len)?;
            array_type.jsonable(&val)
        } else {
            let val = record.ftype.deserialize(buf, offset, self.size_store_len)?;
            record.ftype.jsonable(&val)
        };

        Ok(json!({"Record": Value::Object(record_meta), "Data": data}))
    }

    fn decode_records(
        &self,
        buf: &[u8],
        offset: &mut usize,
        data_size: usize,
    ) -> Result<Vec<Value>, String> {
        let start = *offset;
        let mut records = Vec::new();
        while *offset - start < data_size {
            records.push(self.decode_record(buf, offset)?);
        }
        Ok(records)
    }

    fn is_compression_record(record: &Value) -> bool {
        record["Record"]["record_name"]
            .as_str()
            .map(|name| name.ends_with("dpCompressProc.CompressionRecord"))
            .unwrap_or(false)
    }

    /// Decompress compression records into raw record bytes; returns None if
    /// any record is not a compression record
    fn decompress_records(&self, records: &[Value]) -> Result<Option<Vec<u8>>, String> {
        let metadata_type = self.dictionary.resolve("Svc.CompressionMetadata")?;
        let metadata_size = metadata_type.max_size(self.size_store_len);
        let mut uncompressed = Vec::new();
        for record in records {
            if !Self::is_compression_record(record) {
                return Ok(None);
            }
            let values = record["Data"]["values"]
                .as_array()
                .ok_or("Compression record has no values")?;
            let bytes: Vec<u8> = values
                .iter()
                .map(|v| v.as_u64().unwrap_or(0) as u8)
                .collect();
            let mut meta_offset = 0;
            let metadata =
                metadata_type.deserialize(&bytes, &mut meta_offset, self.size_store_len)?;
            let payload = &bytes[metadata_size..];
            match metadata["algorithm"].as_str() {
                Some("UNCOMPRESSED") => uncompressed.extend_from_slice(payload),
                Some("ZLIB_DEFLATE") => {
                    let mut decoder = ZlibDecoder::new(payload);
                    let mut out = Vec::new();
                    decoder
                        .read_to_end(&mut out)
                        .map_err(|e| format!("Decompression failed: {}", e))?;
                    uncompressed.extend_from_slice(&out);
                }
                other => {
                    return Err(format!(
                        "Compression algorithm {} unsupported",
                        other.unwrap_or("<unknown>")
                    ))
                }
            }
        }
        Ok(Some(uncompressed))
    }

    /// Decode an entire data product file's contents
    pub fn decode(&self, buf: &[u8], disable_decompression: bool) -> Result<Value, String> {
        let mut offset = 0;
        let header = self.decode_header(buf, &mut offset)?;

        let data_size = header["DataSize"]["value"].as_u64().unwrap_or(0) as usize;
        let data_start = offset;
        let mut records = self.decode_records(buf, &mut offset, data_size)?;

        // Validate data checksum
        let stored_bytes = take(buf, &mut offset, CHECKSUM_LEN)?;
        let stored = u32::from_be_bytes(stored_bytes.try_into().unwrap());
        let computed = crc32(&buf[data_start..data_start + data_size]);
        if stored != computed {
            return Err(format!(
                "CRC mismatch in Data: expected {:#x}, got {:#x}",
                stored, computed
            ));
        }

        // Handle compressed data products
        if !disable_decompression && !records.is_empty() && Self::is_compression_record(&records[0])
        {
            if let Some(uncompressed) = self.decompress_records(&records)? {
                let mut uncomp_offset = 0;
                records =
                    self.decode_records(&uncompressed, &mut uncomp_offset, uncompressed.len())?;
            }
        }

        Ok(json!({"Header": header, "Records": records}))
    }
}
