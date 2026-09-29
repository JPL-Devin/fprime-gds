# fprime-dp-rust Design

## Goal

Demonstrate calling Rust from Python (analogous to C++ bindings via pybind11) by
implementing a Rust variant of the `fprime-dp` data product tools. The Rust
implementation must produce **the same output** as the Python reference
(`fprime_gds.common.dp.decoder` / `validator`) on the existing test data.

## How Rust-in-Python works

[PyO3](https://pyo3.rs) generates a CPython extension module from Rust code
(`#[pymodule]` / `#[pyfunction]`), and [maturin](https://maturin.rs) builds and
packages it as a standard Python wheel. From Python's point of view the result
is a regular importable module — exactly like a pybind11-based C++ extension.

```
Python script            CPython extension (.so)          Rust crate
  import fprime_dp_rust ──► PyO3-generated bindings ──► decoder / validator logic
```

## Architecture

```
rust/fprime-dp-rust/          # Rust crate, built with maturin
├── Cargo.toml
├── pyproject.toml            # maturin build backend; wheel name: fprime-dp-rust
└── src/
    ├── lib.rs                # PyO3 module `fprime_dp_rust`: decode(), validate()
    ├── dictionary.rs         # JSON dictionary parsing (typeDefinitions, records)
    ├── types.rs              # F´ type model: deserialize + to_jsonable equivalent
    ├── decoder.rs            # header/record decoding, CRC checks, decompression
    └── validator.rs          # header/data checksum validation (dict/size/guess)

src/fprime_gds/executables/data_products_rust.py   # `fprime-dp-rust` CLI wrapper
test/fprime_gds/common/dp/test_rust.py             # parity tests vs Python reference
```

### Exposed Python API (module `fprime_dp_rust`)

- `decode_to_json(dictionary_path, bin_file_path, disable_decompression=False) -> str`
  Returns the decode result as a JSON string with the same structure
  (`{"Header": ..., "Records": [...]}`) as the Python `DataProductDecoder.decode()`.
  Raises `ValueError` with `CRC mismatch ...` / `Record ID ... not found` messages
  mirroring the Python exceptions.
- `validate(bin_file_path, dictionary_path=None, header_size=None, guess_size=False, verbose=False) -> bool`
  Mirrors `DataProductValidator.process()`.

The thin CLI wrapper `fprime-dp-rust` mirrors the `fprime-dp` argparse interface
(`decode -b -d -o -z` / `validate -b -d -s -g -v`) and writes the JSON output
file with `json.dump(..., indent=2)` for byte-similar files.

### Rust internals

- **Dictionary parsing** (`serde_json`): resolve `typeDefinitions`
  (alias/struct/enum/array) and `records` into a type graph, matching the
  semantics of `fprime_gds.common.loaders.json_loader.parse_type`.
- **Type model**: one Rust enum `FType` (integers, floats, bool, string, enum,
  array, struct) with `deserialize()` and two JSON renderings that replicate the
  Python `to_jsonable()` and `.val` representations (including `"type"` names
  produced by Python's `repr`, e.g. `U32`, and `record_type` strings such as
  `<class 'abc.Svc.DpTool.Complex'>` for dictionary-constructed types).
- **Header**: fixed field sequence per `common.get_dp_header_type()`
  (PacketDescriptor, Id, Priority, Time, ProcTypes, UserData, DpState, DataSize,
  Checksum), with sizes resolved from the dictionary aliases.
- **CRC32**: `crc32fast` (same polynomial/init as Python `binascii.crc32`).
- **Decompression**: `flate2` for `ZLIB_DEFLATE` compression records
  (`Svc.CompressionMetadata`), matching the Python decompress-and-reparse flow.

## Testing strategy (parity loop)

`test/fprime_gds/common/dp/test_rust.py`:
1. For every `.bin`/`.fdp` in `test_dp_data`, run the Python `DataProductDecoder`
   and `fprime_dp_rust.decode_to_json`, and assert deep-equality of the parsed
   JSON (reference output parity).
2. Mirror the validator test matrix (dict / explicit size / guess / corrupted
   files) and assert identical pass/fail results.
3. Error cases: corrupted CRC files must fail in both implementations.

Tests are skipped with a clear message if `fprime_dp_rust` is not built.
Build locally with:

```
pip install maturin
maturin develop -m rust/fprime-dp-rust/Cargo.toml --release
```

## Non-goals

- No change to the existing Python implementation.
- The Rust wheel is an optional add-on; `fprime-gds` does not depend on it.
