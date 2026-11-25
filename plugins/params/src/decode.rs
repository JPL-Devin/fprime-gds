//! Reading a `.dat` file.

use crate::{checksum, Param, ParamDictionary, DELIMITER, ID_SIZE};
use fgds_core::dictionary::Reader;
use fgds_core::serde_json::Value;

fn be32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |acc, b| (acc << 8) | u32::from(*b))
}

pub(crate) fn from_dat<'a>(dict: &'a ParamDictionary, dat: &[u8]) -> Result<Vec<(&'a Param, Value)>, String> {
    if dat.len() < 4 {
        return Err(format!("File too small to contain CRC header: expected at least 4 bytes, got {}", dat.len()));
    }
    let (file_crc, data) = (be32(&dat[..4]), &dat[4..]);
    let computed = checksum(data);
    if file_crc != computed {
        return Err(format!("CRC mismatch: file header has 0x{file_crc:08x}, computed 0x{computed:08x}"));
    }
    let mut params = Vec::new();
    // Offsets in messages count from the start of the file.
    let mut offset = 0;
    while offset < data.len() {
        if data[offset] != DELIMITER {
            let (at, found) = (offset + 4, data[offset]);
            return Err(format!("Invalid delimiter at offset {at}: expected 0xA5, got {found:#x}"));
        }
        offset += 1;
        let field = |offset: usize, what: &str| {
            data.get(offset..offset + 4).map(be32).ok_or_else(|| {
                format!("Incomplete {what} at offset {}: expected 4 bytes, got {}", offset + 4, data.len() - offset)
            })
        };
        let record_size = field(offset, "record size")? as usize;
        offset += 4;
        let id = field(offset, "parameter ID")?;
        offset += 4;
        // Among parameters with the same identifier, the last.
        let Some(param) = dict.params.iter().rev().find(|p| p.id == id) else {
            return Err(format!("Unknown parameter ID {id} (0x{id:x}) at offset {}", offset - 4));
        };
        let name = param.full_name();
        let Some(value_size) = record_size.checked_sub(ID_SIZE) else {
            return Err(format!(
                "Record of {name} at offset {}: size {record_size} is less than the identifier",
                offset - 4
            ));
        };
        let left = data.len() - offset;
        if value_size > left {
            let at = offset + 4;
            return Err(format!(
                "Incomplete parameter value for {name} at offset {at}: expected {value_size} bytes, got {left}"
            ));
        }
        let mut reader = Reader::new(&data[offset..offset + value_size]);
        let decoded = dict.types.decode(&param.ty, &mut reader).map_err(|e| {
            format!("Failed to deserialize parameter {name} (id={id}, type={}): {}", param.ty.name(), e.0)
        })?;
        let decoded = fgds_core::dictionary::exact(&param.ty, decoded);
        params.push((param, decoded));
        offset += value_size;
    }
    Ok(params)
}
