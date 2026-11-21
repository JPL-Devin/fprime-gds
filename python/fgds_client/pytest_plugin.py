"""pytest plugin: the `fprime_test_api` fixture over a running `fgds`.

Options: `--fgds-url` (default `http://127.0.0.1:5000`, or `$FGDS_URL`),
`--fgds-logs` (directory of the test log) and `--dictionary` (path reported
by `fprime_test_api.dictionaries.dictionary_path`; fetched from the server
when not given) and `--deployment-config` (the JSON file `get_mnemonic` reads).

Test files written for the Python GDS import `fprime_gds.common.testing_fw.predicates` and
`fprime_gds.common.utils.event_severity`; with this plugin those names give the modules of
`fgds_client`, so the files run unchanged.
"""

import argparse
import itertools
import importlib
import os
import sys
import types

import pytest

from . import data, predicates
from .client import DEFAULT_URL
from .test_api import TestApi

SEQUENCE_COUNTER = itertools.count()


def _alias(name, module):
    """Make `import name` give `module`, creating empty parent packages where the Python GDS is not installed."""
    parts = name.split(".")
    for depth in range(1, len(parts)):
        parent = ".".join(parts[:depth])
        if parent not in sys.modules:
            try:
                importlib.import_module(parent)
            except ImportError:
                package = types.ModuleType(parent)
                package.__path__ = []
                sys.modules[parent] = package
                if depth > 1:
                    setattr(sys.modules[".".join(parts[:depth - 1])], parts[depth - 1], package)
    sys.modules[name] = module
    setattr(sys.modules[".".join(parts[:-1])], parts[-1], module)


def pytest_configure(config):
    _alias("fprime_gds.common.testing_fw.predicates", predicates)
    _alias("fprime_gds.common.utils.event_severity", data)


def pytest_addoption(parser):
    group = parser.getgroup("fgds")
    group.addoption("--fgds-url", default=os.environ.get("FGDS_URL", DEFAULT_URL), help="address of the running fgds [default: %(default)s]")
    group.addoption("--fgds-logs", default=None, help="directory for the test log (commands, assertions, matched records)")
    try:
        group.addoption("--dictionary", default=None, help="path of the dictionary the server loaded")
    except (ValueError, argparse.ArgumentError):
        pass  # another plugin (the Python GDS) already declares --dictionary
    try:
        group.addoption("--deployment-config", default=None, help="JSON file naming the components of the deployment")
    except (ValueError, argparse.ArgumentError):
        pass


@pytest.fixture(scope="session")
def fprime_test_api_session(request):
    option = request.config.getoption
    api = TestApi(option("--fgds-url"), logpath=option("--fgds-logs"), dictionary_path=option("--dictionary", None),
                  deployment_config=option("--deployment-config", None))
    api.setup()
    try:
        yield api
    finally:
        api.teardown()


@pytest.fixture(scope="function")
def fprime_test_api(fprime_test_api_session, request):
    fprime_test_api_session.start_test_case(request.node.name, next(SEQUENCE_COUNTER))
    return fprime_test_api_session
