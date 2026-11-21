# Ground-truth fixtures

Reference inputs and expected outputs taken from the Python F Prime GDS. The Rust GDS is tested
against them (design: `docs/design/requirements-and-architecture.md`, section 6.1).

- Source repository: `JPL-Devin/fprime-gds`, branch `devel`, commit `de71b640e9ad95d5d4e0b4aa49fc954de873575b`.
- Tool versions: `VERSIONS.txt` (Python 3.12.16, fprime-tools 4.4.0, pytest 9.2.0, lark 1.3.1; the
  file lists every installed package).
- Nothing here is written by hand except this file and `tools/`. Everything else is either copied
  unchanged from the Python repository or produced by running the Python code.

## Regenerating

```sh
git clone -b devel https://github.com/JPL-Devin/fprime-gds python-gds
git -C python-gds checkout de71b640e9ad95d5d4e0b4aa49fc954de873575b
python3 -m venv python-gds/.venv
python-gds/.venv/bin/pip install -e python-gds pytest
fixtures/tools/regenerate.sh python-gds
git status --short fixtures     # prints nothing: the output is byte-identical
```

`tools/regenerate.sh` deletes `dp/`, `sequences/`, `dictionary/` and `vectors/`, then:

| Step | Command | Produces |
|---|---|---|
| 1 | `cp` from `test/fprime_gds/common/...` | copied inputs (below) |
| 2 | `python tools/gen_dp_expected.py fixtures` | `dp/expected/` |
| 3 | `python tools/gen_vectors.py fixtures` | `vectors/*.json` except `ccsds.json`, `sequences/manifest.json`, `dictionary/RefTopologyDictionary.expected.json` |
| 4 | `pytest -p ccsds_recorder test/fprime_gds/common/communication/ccsds` (run in the Python checkout, `PYTHONPATH=fixtures/tools`, `CCSDS_RECORDER_OUTPUT=fixtures/vectors/ccsds.json`) | `vectors/ccsds.json` |
| 5 | `python tools/verify_dp.py fixtures` | nothing; fails if a data product expectation contradicts a Python test assertion |
| 6 | `python tools/versions.py python-gds` | `VERSIONS.txt` |

The output does not depend on the machine, the working directory or `PYTHONHASHSEED` (checked with
two seeds and from a second clone with a new virtual environment).

## Fixture sets

| Directory | Copied from (`test/fprime_gds/common/`) | Content |
|---|---|---|
| `dp/test_dp_data/` | `dp/test_dp_data/` | 18 data products, 2 dictionaries |
| `dp/expected/` | generated | `fprime-dp` results: 18 products x 10 modes, 22 command-line cases |
| `sequences/input/` | `tools/input/` | 37 sequences, 2 parameter JSON files |
| `sequences/expected/` | `tools/expected/` | 16 sequence binaries, 20 error texts, 1 parameter database |
| `sequences/resources/` | `tools/resources/` | the 2 JSON dictionaries the sequences need, 1 XML dictionary |
| `sequences/manifest.json` | generated | input, dictionary and result of each of the 37 sequences |
| `dictionary/RefTopologyDictionary.json` | `loaders/resources/` | Ref dictionary |
| `dictionary/RefTopologyDictionary.expected.json` | generated | what the Python loaders build from it |
| `vectors/` | generated | byte vectors (below) |

### Data products (`dp/`)

`dictionary.json` goes with the `.bin` products, `dictionary_ref.json` with the `.fdp` products.

`dp/expected/<product>/expected.json` holds one entry per mode in `runs`:

```json
{"mode": "decode", "args": ["decode", "-b", "test_dp_data/makeU32.bin", "-d", "test_dp_data/dictionary.json", "-o", "out.json"],
 "exit_code": 0, "stdout": "...", "stderr": "", "output_json": "decode.json"}
```

- `args` are the arguments given to `fprime-dp`, run in a scratch directory that holds a copy of
  `test_dp_data/`. Where the tool prints the scratch directory, it is written `<WORKDIR>`.
- `output_json` names the file next to `expected.json` with the exact bytes the tool wrote; `null`
  when the tool wrote none (failure, or a validate mode).
- `header_size` is the header size the tool computes from the dictionary; `sha256` is the product file.

Modes: `decode`, `decode_no_decompression` (`-z`), `validate_dictionary` (`-d`), `validate_header_size`
(`-s`), `validate_wrong_header_size` (`-s` header size + 3, as the Python test does), `validate_guess`
(`-g`), `validate_no_option`, and the `-v` variant of the three validate modes.

`dp/expected/cli_cases.json` holds the behaviors that are not per product (same fields, plus the
files each command creates with their SHA-256): default output name, `-o`, `--rename-id`, directory
decode (`-B`), continuation after a failure, output name collision, missing file, empty directory,
argument errors (exit code 2).

The two failure products:

| Product | `decode` | `validate` |
|---|---|---|
| `CRC_HEADER_FAILURE_EXPECTED.bin` | exit 1, `Error: CRC mismatch in Header: expected 0x9178ab44, got 0x9178abdb` | exit 1, `Invalid Header checksum. ...` |
| `CRC_FAILURE_EXPECTED.bin` | exit 1, `Error: Record ID 3707770120 not found in dictionary` | exit 1, `Invalid Data checksum. ...` |

The Python decoder does not check the data checksum before decoding records, so the corrupted
data of `CRC_FAILURE_EXPECTED.bin` surfaces as an unknown record, not as a checksum error. The
Python test accepts either error. The fixture records what the tool does.

### Sequences (`sequences/`)

`manifest.json` lists, for each `.seq` input, the dictionary the Python test compiles it with, and
the result of `seqgen.generateSequence(input, output, dictionary, 0xffff)`:

- `result: binary`: the output is byte-identical to `expected_file`.
- `result: error`: `python_error` is the full message, and the text of `expected_file` is contained
  in it (this is what the Python test checks). The input directory is written
  `<fprime-gds>/test/fprime_gds/common/tools/input`, as in the Python expected files.
- `multiple_errors.seq` has no expected file in the Python repository (the test checks that the
  message contains `Encountered syntax error`); `continue_on_error` is true for it.

`simple_paramdb.json`, `simple_bad_paramdb.json` and `expected/simple_paramdb.dat` are parameter
files; their vectors are in `vectors/params.json`.

### Byte vectors (`vectors/`)

Every file is `{"description": ..., "vectors": [...]}` (`ccsds.json`: `"cases"`). All byte strings
are lowercase hex without separators. Every vector has:

- `source`: the Python test it comes from.
- `origin`: `asserted` when the Python test compares against these same literal bytes; `generated`
  when the test checks a round trip or a property only and the bytes were produced by running the
  Python code. The generator fails if the Python code does not reproduce an `asserted` value.
- `operation`: what to run (listed in each file's `description`).
- the input fields, and `expected` or `expected_error` (`type` and `message` of the Python exception;
  the message text is informative, a Rust error does not have to match it).

Types are described as `{"kind": "U32"}` (`U8`..`U64`, `I8`..`I64`, `F32`, `F64`, `bool`), or with
`kind` `enum` (`representation`, `members`), `string` (`max_length`), `array` (`length`, `member`),
`struct` (`members`: `name`, `type`), `time`.

| File | Vectors | Content |
|---|---|---|
| `types.json` | 186 | serialization of every F Prime type: 88 value <-> bytes, 76 rejected values, 21 rejected byte strings, 1 bytes -> value |
| `encoders.json` | 28 | channel, event, telemetry packet, command, file packet encoders, each with a `U32` and a `U16` length prefix |
| `decoders.json` | 17 | channel, event, telemetry packet, file packet decoders; message splitting and routing |
| `fprime_framing.json` | 31 | F Prime frame (`deadbeef`), CRC-32 and fixed checksum; partial frames, garbage, bad checksum; TCP server framing |
| `ccsds.json` | 75 cases, 324 steps | Space Packet, Space Packet splitter, TM/TC Space Data Link, TM frame aggregator, and the three plugin chains |
| `params.json` | 10 | parameter database encoding, decoding (JSON, text, CSV) and the 5 decode errors |
| `format_strings.json` | 41 | C style and FPP style format strings: 25 asserted by Python tests, 16 recorded from the Python formatter |

`ccsds.json` has one case per Python test: `instances` (class and configuration of each framer the
test created) and `steps` (`instance`, `operation`, `input`, `expected`, `warnings`), in call order.
State carries across the steps of an instance. Only calls made by the test itself are recorded;
calls between framers are part of the outer call.

## Decisions

- Expected outputs of `fprime-dp` are kept in `dp/expected/<product>/` and not beside the product
  file, so that `dp/test_dp_data/` stays an unchanged copy and directory decoding (`-B`) of it still
  sees 18 products.
- Error cases keep the Python exception type and message. They are the exact text for `fprime-dp`
  (it prints them); for library errors they are a reference, not a contract.
- Round-trip-only tests (types, F Prime framing, commands, file packets) were turned into concrete
  bytes by running the Python code. Cases added beyond the Python tests are marked in `source` with
  `(none: ...)`: a command with no argument, a command with every remaining argument type, file
  packets, TCP server framing, enum representations other than `I32`, non-ASCII and empty strings,
  truncated input for each type.
- `BoolType` deserialization: only `ff` is true and only `00` is false in the Python code; other
  byte values are recorded as rejected or accepted exactly as Python does.
- `F32` values: `value` is the input, `deserialized_value` (when present) is what comes back after
  the round trip through 4 bytes.
- The XML dictionary in `sequences/resources/` is copied for completeness; no generated expectation uses it.

## Not turned into fixtures

| Python tests | Reason |
|---|---|
| `communication/adapters/test_tcp_fast.py`, `test_udp_fast.py`, the socket and serial paths of `bridge/test_comm_bridge.py` | They test live sockets, threads and timing. The bytes they move are covered by `fprime_framing.json`; the transport behavior has no file form. |
| `ccsds/test_ccsds_chains.py` SHA-256 reference digests | The digests cover the packet lists; the packet lists themselves are recorded in `ccsds.json`, which is stronger. |
| `ccsds` tests of configuration from the dictionary, command-line arguments and plugin registration (26 of 101 tests; they make no framer call) | No bytes involved. The accepted and rejected frame size and spacecraft ID values are Python argument handling. |
| `models/serialize/test_types.py`: `test_base_type`, `test_dictionary_type_errors`, and rejected values of Python-only kinds (`None`, objects, classes) | They test the Python class hierarchy and the Python type registry. |
| `models/serialize/time_type_unit_test.py` | Comparison and arithmetic operators of the Python time class; no serialization. |
| `history/`, `testing_fw/`, `gds_cli/`, `executables/test_cli.py`, `test_run_deployment.py`, `test_utils.py`, `test_plugins.py`, `flask/` | Python API, command-line parser, plugin system and web server behavior. They belong to the scripted end-to-end comparison of section 6.1, not to byte fixtures. |
| `gds_cli/file_uplink_test.py` | Uses mocks; it holds no packet bytes. File packets are in `encoders.json` and `decoders.json`, generated. |
| `tools/test_lark_transformer.py` | Tests the Python parse tree. The observable result (binary or error per sequence) is in `sequences/`. |
| `tools/test_prm_decode.py` `test_encoder_format_*`, `test_params_to_json_multiple_components` | They build Python template objects by hand to test a JSON reshaping helper; decoding of real files is in `params.json`. |
| `executables/test_dictionary_merge.py` | Dictionary merge tool, built on temporary dictionaries created in the test; outside the listed sets. |
| `yamcs_transport_test.py` | Name mapping and mocks for an optional YAMCS transport; no bytes. |
| `data_types/test_cmd_data.py` argument error strings | The valid arguments are in `encoders.json`; the string rendering of a Python object is not. |
| A capture of the live `Ref` deployment (section 6.1 scenario) | Needs a built `Ref` deployment; not available from the Python test suite. |
