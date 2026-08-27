//! Data product validator, mirroring `fprime_gds.common.dp.validator`.

use crate::decoder::{crc32, CHECKSUM_LEN};

pub struct ValidationOutput {
    pub ok: bool,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
}

fn validate_payload_checksum(payload: &[u8]) -> (bool, u32, u32) {
    let data = &payload[..payload.len() - CHECKSUM_LEN];
    let stored = u32::from_be_bytes(payload[payload.len() - CHECKSUM_LEN..].try_into().unwrap());
    let computed = crc32(data);
    (stored == computed, stored, computed)
}

/// Validate header and data checksums with a given header size.
/// Returns (ok, failing_section, (stored, computed))
fn validate_data_product(buf: &[u8], header_size: usize) -> (bool, &'static str, (u32, u32)) {
    let (ok, stored, computed) = validate_payload_checksum(&buf[..header_size]);
    if !ok {
        return (false, "Header", (stored, computed));
    }
    let (ok, stored, computed) = validate_payload_checksum(&buf[header_size..]);
    if !ok {
        return (false, "Data", (stored, computed));
    }
    (true, "", (0, 0))
}

pub fn validate(
    buf: &[u8],
    header_size: Option<usize>,
    dictionary_header_size: Option<usize>,
    verbose: bool,
) -> ValidationOutput {
    let mut out = ValidationOutput {
        ok: false,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };

    // See validate_with_guess in the Python implementation
    let min_header_size = 1 + 2 + 2 + 8 + 1 + 1 + 2 + 4; // 21 bytes
    let mut min_dp_size = min_header_size + 1 + 4;
    if buf.len() < min_dp_size {
        out.stderr.push(format!(
            "Data Product file size below minimum {}",
            min_dp_size
        ));
        return out;
    }

    if let Some(size) = header_size {
        min_dp_size = size + 1 + 4;
        if buf.len() < min_dp_size {
            out.stderr.push(format!(
                "Data Product file size below minimum {}",
                min_dp_size
            ));
            return out;
        }
        let (ok, failure, checksums) = validate_data_product(buf, size);
        if !ok {
            out.stderr.push(format!(
                "Invalid {} checksum. Checksum in file {:08x}. Calculated Checksum {:08x}",
                failure, checksums.0, checksums.1
            ));
            return out;
        }
    } else if let Some(size) = dictionary_header_size {
        if verbose {
            out.stdout
                .push(format!("Calculated a header size of {}", size));
        }
        let (ok, failure, checksums) = validate_data_product(buf, size);
        if !ok {
            out.stderr.push(format!(
                "Invalid {} checksum. Checksum in file {:08x}. Calculated Checksum {:08x}",
                failure, checksums.0, checksums.1
            ));
            return out;
        }
    } else {
        // Guess the header size
        let default_guess_size = 2 + 4 + 4 + 11 + 1 + 32 + 1 + 8 + 4; // 67 bytes
        let min_guess_size = 1 + 2 + 2 + 8 + 1 + 1 + 2 + 4; // 21 bytes
        let mut max_guess_size = 4 + 8 + 8 + 11 + 1 + 256 + 1 + 8 + 4; // 301 bytes

        let max_header_size = buf.len() - (CHECKSUM_LEN + 1);
        max_guess_size = max_guess_size.min(max_header_size);

        let mut found = false;
        if default_guess_size <= max_guess_size {
            let (ok, _, _) = validate_data_product(buf, default_guess_size);
            if ok {
                if verbose {
                    out.stdout.push(format!(
                        "Valid checksum found with default size {}",
                        default_guess_size
                    ));
                }
                found = true;
            }
        }
        if !found {
            for guess in min_guess_size..=max_guess_size {
                let (ok, _, _) = validate_data_product(buf, guess);
                if ok {
                    if verbose {
                        out.stdout
                            .push(format!("Valid checksum found with header size {}", guess));
                    }
                    found = true;
                    break;
                }
            }
        }
        if !found {
            out.stderr.push(format!(
                "No valid checksum found with header sizes in range [{},{}]",
                min_guess_size, max_guess_size
            ));
            return out;
        }
    }

    out.ok = true;
    out.stdout.push("Validation OK!".to_string());
    out
}
