"""
Parity tests for the fprime_dp_rust extension (rust/fprime-dp-rust)

Verifies that the Rust implementation of the data product decoder and
validator produces output identical to the Python reference implementation
(fprime_gds.common.dp.decoder / validator) on the shared test data.

Skipped entirely when the fprime_dp_rust extension is not installed
(build it with `maturin develop` or `pip install ./rust/fprime-dp-rust`).
"""

import json
from pathlib import Path

import pytest

fprime_dp_rust = pytest.importorskip("fprime_dp_rust")

from fprime_gds.common.dp.decoder import DataProductDecoder, DataProductError
from fprime_gds.common.dp.validator import DataProductValidator
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.utils.cleanup import globals_cleanup


TEST_DATA_DIR = Path(__file__).parent / "test_dp_data"
DICTIONARY_PATH = TEST_DATA_DIR / "dictionary.json"
DICTIONARY_REF_PATH = TEST_DATA_DIR / "dictionary_ref.json"

BIN_FILES = [
    "makeBool.bin",
    "makeComplex.bin",
    "makeDataArray.bin",
    "makeEnum.bin",
    "makeF32.bin",
    "makeF64.bin",
    "makeFppArray.bin",
    "makeI16.bin",
    "makeI32.bin",
    "makeI64.bin",
    "makeI8.bin",
    "makeU32.bin",
    "makeU32Array.bin",
    "makeU8Array.bin",
]

FDP_FILES = [
    "DpDemoUncompressed.fdp",
    "DpDemoCompressed.fdp",
]


@pytest.fixture
def load_dictionary():
    globals_cleanup()
    dictionaries = Dictionaries.load_dictionaries_into_config(str(DICTIONARY_PATH))
    yield dictionaries
    globals_cleanup()


@pytest.fixture
def load_dictionary_ref():
    globals_cleanup()
    dictionaries = Dictionaries.load_dictionaries_into_config(str(DICTIONARY_REF_PATH))
    yield dictionaries
    globals_cleanup()


def python_reference_decode(dictionaries, bin_file, tmp_path, disable_decompression=False):
    """Decode with the Python reference implementation, normalized through
    JSON exactly as `DataProductDecoder.process()` serializes it."""
    decoder = DataProductDecoder(
        dictionaries,
        str(bin_file),
        str(tmp_path / "ref.json"),
        disable_decompression,
    )
    return json.loads(json.dumps(decoder.decode(), default=str))


class TestDecodeParity:
    """Rust decode output must be identical to the Python reference."""

    @pytest.mark.parametrize("bin_name", BIN_FILES)
    def test_decode_parity(self, load_dictionary, tmp_path, bin_name):
        bin_file = TEST_DATA_DIR / bin_name
        expected = python_reference_decode(load_dictionary, bin_file, tmp_path)
        actual = json.loads(
            fprime_dp_rust.decode_to_json(str(DICTIONARY_PATH), str(bin_file))
        )
        assert actual == expected

    @pytest.mark.parametrize("fdp_name", FDP_FILES)
    def test_decode_parity_fdp(self, load_dictionary_ref, tmp_path, fdp_name):
        fdp_file = TEST_DATA_DIR / fdp_name
        expected = python_reference_decode(load_dictionary_ref, fdp_file, tmp_path)
        actual = json.loads(
            fprime_dp_rust.decode_to_json(str(DICTIONARY_REF_PATH), str(fdp_file))
        )
        assert actual == expected

    def test_decode_parity_disable_decompression(self, load_dictionary_ref, tmp_path):
        fdp_file = TEST_DATA_DIR / "DpDemoCompressed.fdp"
        expected = python_reference_decode(
            load_dictionary_ref, fdp_file, tmp_path, disable_decompression=True
        )
        actual = json.loads(
            fprime_dp_rust.decode_to_json(
                str(DICTIONARY_REF_PATH), str(fdp_file), True
            )
        )
        assert actual == expected

    def test_compressed_matches_uncompressed(self, load_dictionary_ref):
        compressed = json.loads(
            fprime_dp_rust.decode_to_json(
                str(DICTIONARY_REF_PATH), str(TEST_DATA_DIR / "DpDemoCompressed.fdp")
            )
        )
        uncompressed = json.loads(
            fprime_dp_rust.decode_to_json(
                str(DICTIONARY_REF_PATH), str(TEST_DATA_DIR / "DpDemoUncompressed.fdp")
            )
        )
        assert compressed["Records"] == uncompressed["Records"]

    def test_corrupted_data_crc_fails_both(self, load_dictionary, tmp_path):
        bin_file = TEST_DATA_DIR / "CRC_FAILURE_EXPECTED.bin"
        with pytest.raises(DataProductError):
            python_reference_decode(load_dictionary, bin_file, tmp_path)
        with pytest.raises(ValueError):
            fprime_dp_rust.decode_to_json(str(DICTIONARY_PATH), str(bin_file))

    def test_corrupted_header_crc_fails_both(self, load_dictionary, tmp_path):
        bin_file = TEST_DATA_DIR / "CRC_HEADER_FAILURE_EXPECTED.bin"
        with pytest.raises(DataProductError):
            python_reference_decode(load_dictionary, bin_file, tmp_path)
        with pytest.raises(ValueError, match="CRC mismatch in Header"):
            fprime_dp_rust.decode_to_json(str(DICTIONARY_PATH), str(bin_file))


class TestValidateParity:
    """Rust validation verdicts must match the Python reference."""

    def python_validate(self, bin_file, dictionary=None, header_size=None, guess_size=False):
        validator = DataProductValidator(
            dictionary=dictionary,
            header_size=header_size,
            guess_size=guess_size,
        )
        return validator.process(str(bin_file))

    def rust_validate(self, bin_file, dictionary=None, header_size=None):
        ok, _, _ = fprime_dp_rust.validate(
            str(bin_file),
            str(dictionary) if dictionary else None,
            header_size,
        )
        return ok

    @pytest.mark.parametrize("bin_name", BIN_FILES)
    def test_validate_with_dictionary(self, load_dictionary, bin_name):
        bin_file = TEST_DATA_DIR / bin_name
        expected = self.python_validate(bin_file, dictionary=str(DICTIONARY_PATH))
        actual = self.rust_validate(bin_file, dictionary=DICTIONARY_PATH)
        assert actual == expected is True

    def test_validate_with_explicit_size(self, load_dictionary):
        bin_file = TEST_DATA_DIR / "makeU32.bin"
        expected = self.python_validate(bin_file, header_size=63)
        actual = self.rust_validate(bin_file, header_size=63)
        assert actual == expected is True

    def test_validate_with_wrong_size(self, load_dictionary):
        bin_file = TEST_DATA_DIR / "makeU32.bin"
        expected = self.python_validate(bin_file, header_size=30)
        actual = self.rust_validate(bin_file, header_size=30)
        assert actual == expected is False

    @pytest.mark.parametrize("bin_name", BIN_FILES + FDP_FILES)
    def test_validate_with_guess(self, bin_name):
        bin_file = TEST_DATA_DIR / bin_name
        expected = self.python_validate(bin_file, guess_size=True)
        actual = self.rust_validate(bin_file)
        assert actual == expected is True

    @pytest.mark.parametrize(
        "bin_name",
        ["CRC_FAILURE_EXPECTED.bin", "CRC_HEADER_FAILURE_EXPECTED.bin"],
    )
    def test_validate_corrupted(self, load_dictionary, bin_name):
        bin_file = TEST_DATA_DIR / bin_name
        expected = self.python_validate(bin_file, dictionary=str(DICTIONARY_PATH))
        actual = self.rust_validate(bin_file, dictionary=DICTIONARY_PATH)
        assert actual == expected is False
