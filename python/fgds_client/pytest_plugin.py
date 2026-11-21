"""pytest plugin: the `fprime_test_api` fixture over a running `fgds`.

Options: `--fgds-url` (default `http://127.0.0.1:5000`, or `$FGDS_URL`),
`--fgds-logs` (directory of the test log) and `--dictionary` (path reported
by `fprime_test_api.dictionaries.dictionary_path`; fetched from the server
when not given).
"""

import argparse
import itertools
import os

import pytest

from .client import DEFAULT_URL
from .test_api import TestApi

SEQUENCE_COUNTER = itertools.count()


def pytest_addoption(parser):
    group = parser.getgroup("fgds")
    group.addoption("--fgds-url", default=os.environ.get("FGDS_URL", DEFAULT_URL), help="address of the running fgds [default: %(default)s]")
    group.addoption("--fgds-logs", default=None, help="directory for the test log (commands, assertions, matched records)")
    try:
        group.addoption("--dictionary", default=None, help="path of the dictionary the server loaded")
    except (ValueError, argparse.ArgumentError):
        pass  # another plugin (the Python GDS) already declares --dictionary


@pytest.fixture(scope="session")
def fprime_test_api_session(request):
    option = request.config.getoption
    api = TestApi(option("--fgds-url"), logpath=option("--fgds-logs"), dictionary_path=option("--dictionary", None))
    api.setup()
    try:
        yield api
    finally:
        api.teardown()


@pytest.fixture(scope="function")
def fprime_test_api(fprime_test_api_session, request):
    fprime_test_api_session.start_test_case(request.node.name, next(SEQUENCE_COUNTER))
    return fprime_test_api_session
