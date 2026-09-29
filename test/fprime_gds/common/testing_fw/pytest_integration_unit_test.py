"""Unit tests for pytest_integration.py's config-file/pytest-CLI precedence handling

pytest_addoption() registers the standard pipeline options with the fprime-gds configuration file
(via FPRIME_GDS_CONFIG_PATH) applied as their defaults. These tests assert that:
  * an option left at its default on the pytest command line is filled in from the configuration
    file (including options like --tts-port whose argparse default is not None),
  * an option explicitly given on the pytest command line still takes precedence over the file, and
  * without a configuration file the options keep their real defaults (so `pytest --help` and
    config.getoption() report them).
"""

import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from _pytest.config.argparsing import Parser

from fprime_gds.common.testing_fw.pytest_integration import pytest_addoption
from fprime_gds.executables import cli

UNIT_TEST_DICTIONARY = str(Path(__file__).parent / "UnitTestDictionary.xml")


def _parse_pytest_args(args, config_text=None):
    """Build a pytest-style namespace the same way pytest would parse its command line"""
    with tempfile.TemporaryDirectory() as tmp_dir:
        config_path = Path(tmp_dir) / "fprime-gds.yml"
        environment = {}
        if config_text is not None:
            config_path.write_text(config_text)
            environment[cli.CONFIGURATION_PATH_ENV] = str(config_path)
        with mock.patch.dict(os.environ, environment):
            parser = Parser(_ispytest=True)
            pytest_addoption(parser)
    # Supply a dictionary explicitly so DictionaryParser does not try to auto-detect a deployment
    # (irrelevant to what is under test here).
    return parser.parse(["--dictionary", UNIT_TEST_DICTIONARY] + args)


class TestPytestIntegrationConfigPrecedence(unittest.TestCase):
    def test_defaulted_option_is_filled_in_from_config_file(self):
        """A pytest option left at its default should be overridden by the configuration file"""
        namespace = _parse_pytest_args([], "command-line-options:\n  tts-port: 60000\n")
        self.assertEqual(namespace.tts_port, 60000)

    def test_explicit_pytest_flag_wins_over_config_file(self):
        """An option given explicitly on the pytest command line must not be overridden by the file"""
        namespace = _parse_pytest_args(
            ["--tts-addr", "10.0.0.5"],
            "command-line-options:\n  tts-port: 60000\n  tts-addr: 10.0.0.9\n",
        )
        self.assertEqual(namespace.tts_addr, "10.0.0.5")
        self.assertEqual(namespace.tts_port, 60000)

    def test_options_keep_real_defaults_without_config_file(self):
        """Without a configuration file the pytest options carry the standard defaults"""
        real_default = next(
            specifiers["default"]
            for flags, specifiers in cli.all_arguments([cli.STANDARD_PIPELINE]).items()
            if "--tts-port" in flags
        )
        namespace = _parse_pytest_args([])
        self.assertEqual(namespace.tts_port, real_default)


if __name__ == "__main__":
    unittest.main()
