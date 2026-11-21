"""Fixture set-up for the `Ref` integration tests run against `fgds`.

The test files are copies of `TestDeploymentsProject/Ref/test/int` of F Prime with
only their imports changed. Tests that need file transfer or sequences are skipped
until those plugins exist.

Run with `tests/cli/ref_int.py`, or against a running `fgds` and `Ref` with
`pytest -p fgds_client.pytest_plugin --fgds-url http://127.0.0.1:5000` (the `-p`
option is not needed once `python/` is installed with pip).
"""

import pytest

NEEDS = {
    "test_seqgen": "sequences",
    "test_seqgen_complex_args": "sequences",
    "test_file_uplink_sandbox": "file transfer",
}


def pytest_collection_modifyitems(items):
    for item in items:
        if item.name in NEEDS:
            item.add_marker(pytest.mark.skip(reason=f"needs {NEEDS[item.name]}"))
