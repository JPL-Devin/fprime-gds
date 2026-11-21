//! Checksum validation of a data product file, as `fprime-dp validate` does it: the
//! header size comes from the dictionary, from the caller, or is guessed.

use crate::decode::{crc32, CHECKSUM_LEN};

/// Sizes of the smallest and largest header worth guessing, and of the default
/// configuration, which is tried first.
pub const MIN_HEADER: usize = 1 + 2 + 2 + 8 + 1 + 1 + 2 + 4;
const MAX_HEADER: usize = 4 + 8 + 8 + 11 + 1 + 256 + 1 + 8 + 4;
const DEFAULT_HEADER: usize = 2 + 4 + 4 + 11 + 1 + 32 + 1 + 8 + 4;

/// A header, one byte of data and the data checksum.
pub const MIN_PRODUCT: usize = MIN_HEADER + 1 + CHECKSUM_LEN;

/// How the header size is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderSize {
    Given(usize),
    /// Computed from the dictionary.
    Dictionary(usize),
    Guess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// What `fprime-dp validate` prints before it exits with 1.
    Failed(String),
    /// A condition the Python tool does not handle (it ends with a traceback).
    Crash(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    /// The lines the tool prints with `-v`, whatever the result.
    pub verbose: Vec<String>,
    pub result: Result<(), Invalid>,
}

pub fn validate(data: &[u8], header_size: HeaderSize) -> Validation {
    let done = |verbose: Vec<String>, result| Validation { verbose, result };
    let below = |minimum: usize| Err(Invalid::Failed(format!("Data Product file size below minimum {minimum}")));
    if data.len() < MIN_PRODUCT {
        return done(Vec::new(), below(MIN_PRODUCT));
    }
    match header_size {
        HeaderSize::Given(size) if data.len() < size.saturating_add(1 + CHECKSUM_LEN) => {
            done(Vec::new(), below(size.saturating_add(1 + CHECKSUM_LEN)))
        }
        HeaderSize::Given(size) => done(Vec::new(), checksums(data, size)),
        HeaderSize::Dictionary(size) => {
            done(vec![format!("Calculated a header size of {size}")], checksums(data, size))
        }
        HeaderSize::Guess => {
            let largest = MAX_HEADER.min(data.len() - (CHECKSUM_LEN + 1));
            let found =
                |size: usize, what: &str| done(vec![format!("Valid checksum found with {what} size {size}")], Ok(()));
            if DEFAULT_HEADER <= largest && checksums(data, DEFAULT_HEADER).is_ok() {
                return found(DEFAULT_HEADER, "default");
            }
            match (MIN_HEADER..=largest).find(|size| checksums(data, *size).is_ok()) {
                Some(size) => found(size, "header"),
                None => done(
                    Vec::new(),
                    Err(Invalid::Failed(format!(
                        "No valid checksum found with header sizes in range [{MIN_HEADER},{largest}]"
                    ))),
                ),
            }
        }
    }
}

/// The header is the first `header_size` bytes, the data is everything after; each
/// ends with the CRC-32 of what precedes it.
fn checksums(data: &[u8], header_size: usize) -> Result<(), Invalid> {
    if header_size <= CHECKSUM_LEN {
        return Err(Invalid::Crash(format!("Expected Header Size to be at least {CHECKSUM_LEN} bytes")));
    }
    let (header, rest) = data.split_at(header_size.min(data.len()));
    for (section, payload) in [("Header", header), ("Data", rest)] {
        let Some((body, stored)) = payload.split_last_chunk::<CHECKSUM_LEN>() else {
            return Err(Invalid::Crash("unpack requires a buffer of 4 bytes".into()));
        };
        let (stored, computed) = (u32::from_be_bytes(*stored), crc32(body));
        if stored != computed {
            return Err(Invalid::Failed(format!(
                "Invalid {section} checksum. Checksum in file {stored:08x}. Calculated Checksum {computed:08x}"
            )));
        }
    }
    Ok(())
}
