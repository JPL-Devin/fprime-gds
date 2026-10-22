//! PyO3 bindings exposing the Rust data product tools as the Python module
//! `fprime_dp_rust`.

mod decoder;
mod dictionary;
mod types;
mod validator;

use pyo3::exceptions::{PyFileNotFoundError, PyValueError};
use pyo3::prelude::*;

use crate::decoder::Decoder;
use crate::dictionary::Dictionary;

fn read_file(path: &str) -> PyResult<Vec<u8>> {
    std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => PyFileNotFoundError::new_err(format!("{}: {}", path, e)),
        _ => PyValueError::new_err(format!("Unable to read {}: {}", path, e)),
    })
}

/// Decode a data product binary file into a JSON string with the same
/// structure as the Python `DataProductDecoder.decode()` result.
#[pyfunction]
#[pyo3(signature = (dictionary_path, bin_file_path, disable_decompression=false))]
fn decode_to_json(
    dictionary_path: &str,
    bin_file_path: &str,
    disable_decompression: bool,
) -> PyResult<String> {
    let dictionary = Dictionary::load(dictionary_path).map_err(PyValueError::new_err)?;
    let buf = read_file(bin_file_path)?;
    let decoder = Decoder::new(&dictionary).map_err(PyValueError::new_err)?;
    let result = decoder
        .decode(&buf, disable_decompression)
        .map_err(PyValueError::new_err)?;
    serde_json::to_string(&result).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// Validate a data product file's header and data checksums.
/// Returns (ok, stdout_lines, stderr_lines).
#[pyfunction]
#[pyo3(signature = (bin_file_path, dictionary_path=None, header_size=None, verbose=false))]
fn validate(
    bin_file_path: &str,
    dictionary_path: Option<&str>,
    header_size: Option<usize>,
    verbose: bool,
) -> PyResult<(bool, Vec<String>, Vec<String>)> {
    let buf = read_file(bin_file_path)?;
    let dictionary_header_size = match (header_size, dictionary_path) {
        (None, Some(path)) => {
            let dictionary = Dictionary::load(path).map_err(PyValueError::new_err)?;
            let decoder = Decoder::new(&dictionary).map_err(PyValueError::new_err)?;
            Some(decoder.header_size())
        }
        _ => None,
    };
    let output = validator::validate(&buf, header_size, dictionary_header_size, verbose);
    Ok((output.ok, output.stdout, output.stderr))
}

#[pymodule]
fn fprime_dp_rust(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(decode_to_json, m)?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
