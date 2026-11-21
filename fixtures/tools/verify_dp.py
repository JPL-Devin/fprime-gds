#!/usr/bin/env python3
"""Check the committed data product expectations against what the Python test suite asserts.

Usage: verify_dp.py <fixtures dir>

Each check below restates an assertion of the Python tests (named in the comment) on the committed
expectation. In addition every committed decode JSON is compared with the result of the library
call the Python tests use (DataProductDecoder.decode), and every validate exit code with
DataProductValidator.process.
"""
import json
import sys
from pathlib import Path

from fprime_gds.common.dp.decoder import CRCError, DataProductDecoder, RecordNotFoundError
from fprime_gds.common.dp.validator import DataProductValidator
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.utils.cleanup import globals_cleanup

FIXTURES = Path(sys.argv[1]).resolve()
DATA = FIXTURES / "dp" / "test_dp_data"
EXPECTED = FIXTURES / "dp" / "expected"
NOMINAL = [
    "makeBool.bin", "makeComplex.bin", "makeDataArray.bin", "makeEnum.bin", "makeF32.bin", "makeF64.bin", "makeFppArray.bin",
    "makeI16.bin", "makeI32.bin", "makeI64.bin", "makeI8.bin", "makeU32.bin", "makeU32Array.bin", "makeU8Array.bin",
]
checks = 0


def check(condition, message):
    global checks
    checks += 1
    assert condition, message


def load(product):
    expectation = json.loads((EXPECTED / product / "expected.json").read_text())
    return expectation, {run["mode"]: run for run in expectation["runs"]}


def decoded(product, mode="decode"):
    return json.loads((EXPECTED / product / f"{mode}.json").read_text())


products = sorted(path.name for path in DATA.iterdir() if path.suffix in (".bin", ".fdp"))
check(len(products) == 18, "18 product files")
check(sorted(path.name for path in EXPECTED.iterdir() if path.is_dir()) == products, "one expectation per product")

for product in NOMINAL:
    expectation, runs = load(product)
    result = decoded(product)
    # test_decoder.py: test_decode_<type>, test_decode_all_test_files, test_decode_and_verify_json_structure
    check(runs["decode"]["exit_code"] == 0, product)
    check(isinstance(result["Header"], dict) and isinstance(result["Records"], list) and result["Records"], product)
    check(all("Record" in record and "Data" in record for record in result["Records"]), product)
    # test_decoder.py: test_decode_bool, test_decode_header
    check(all(field in result["Header"] for field in ("Id", "DataSize", "Checksum")), product)
    # test_validator.py: *_with_dict, test_validate_multiple_files_with_header_size (63), *_with_guess, test_validate_with_no_validation_method
    check(expectation["header_size"] == 63, product)
    for mode in ("validate_dictionary", "validate_header_size", "validate_guess", "validate_no_option"):
        check(runs[mode]["exit_code"] == 0 and "Validation OK!" in runs[mode]["stdout"], f"{product} {mode}")
    # test_validator.py: test_validate_with_incorrect_header_size (63 + 3)
    check(runs["validate_wrong_header_size"]["exit_code"] == 1, product)
    check(runs["validate_wrong_header_size"]["args"][-1] == "66", product)
    # test_validator.py: test_validate_with_dict_verbose, test_validate_with_guess_verbose
    check("Calculated a header size of" in runs["validate_dictionary_verbose"]["stdout"], product)
    check("Validation OK!" in runs["validate_dictionary_verbose"]["stdout"], product)
    check("Valid checksum found with" in runs["validate_guess_verbose"]["stdout"], product)

# test_decoder.py: test_decode_u8_array, test_decode_u32_array, test_decode_array_record
for product in ("makeU8Array.bin", "makeU32Array.bin"):
    data = decoded(product)["Records"][0]["Data"]
    check(isinstance(data["values"], list) and len(data["values"]) == data["size"], product)

# test_decoder.py: test_decode_corrupted_file, test_decode_corrupted_header (CRCError or RecordNotFoundError)
# test_validator.py: test_validate_corrupted_file, test_failure_expected
# test_data_products.py: test_decode_header_crc_failure_fails, test_decode_record_failure_fails
for product, text in (("CRC_FAILURE_EXPECTED.bin", "not found in dictionary"), ("CRC_HEADER_FAILURE_EXPECTED.bin", "CRC mismatch in Header")):
    expectation, runs = load(product)
    check(runs["decode"]["exit_code"] == 1 and runs["decode"]["output_json"] is None, product)
    check(runs["decode"]["stderr"].startswith("Error: ") and text in runs["decode"]["stderr"], product)
    check(not (EXPECTED / product / "decode.json").exists(), product)
    for mode in ("validate_dictionary", "validate_header_size", "validate_guess"):
        check(runs[mode]["exit_code"] == 1, f"{product} {mode}")
check("Invalid Data checksum" in load("CRC_FAILURE_EXPECTED.bin")[1]["validate_dictionary"]["stderr"], "data checksum failure")
check("Invalid Header checksum" in load("CRC_HEADER_FAILURE_EXPECTED.bin")[1]["validate_dictionary"]["stderr"], "header checksum failure")

# test_decoder.py: test_decode_compressed, test_decode_compressed_raw, test_decode_compressed_compare
check(len(decoded("DpDemoCompressed.fdp")["Records"]) == 14, "14 records")
raw = decoded("DpDemoCompressed.fdp", "decode_no_decompression")["Records"]
check(len(raw) == 1 and raw[0]["Record"]["record_name"] == "DpCompression.dpCompressProc.CompressionRecord", "raw compressed record")
check(decoded("DpDemoUncompressed.fdp")["Records"] == decoded("DpDemoCompressed.fdp")["Records"], "compressed equals uncompressed")

# The library calls the Python tests use give the committed results
for product in products:
    expectation, runs = load(product)
    dictionary = str(DATA / expectation["dictionary"])
    globals_cleanup()
    dictionaries = Dictionaries.load_dictionaries_into_config(dictionary)
    for mode, disable in (("decode", False), ("decode_no_decompression", True)):
        try:
            result = DataProductDecoder(dictionaries, str(DATA / product), "unused.json", disable_decompression=disable).decode()
        except (CRCError, RecordNotFoundError) as exception:
            check(runs[mode]["exit_code"] == 1 and str(exception) in runs[mode]["stderr"], f"{product} {mode}")
        else:
            check(json.loads(json.dumps(result, default=str)) == decoded(product, mode), f"{product} {mode}")
    validators = {
        "validate_dictionary": DataProductValidator(dictionary=dictionary),
        "validate_header_size": DataProductValidator(header_size=expectation["header_size"]),
        "validate_wrong_header_size": DataProductValidator(header_size=expectation["header_size"] + 3),
        "validate_guess": DataProductValidator(guess_size=True),
        "validate_no_option": DataProductValidator(),
    }
    for mode, validator in validators.items():
        check(validator.process(str(DATA / product)) is (runs[mode]["exit_code"] == 0), f"{product} {mode}")
    globals_cleanup()

print(f"verify_dp: {checks} checks passed on {len(products)} data products", file=sys.stderr)
