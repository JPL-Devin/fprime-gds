import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from fprime_gds.executables.cli import ConfigDrivenParser, MiddleWareParser

CONFIGURATION = "command-line-options:\n  tts-port: 60000\n  tts-addr: 10.0.0.5\n"


class TestConfigDrivenParser(unittest.TestCase):

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.config_path = Path(self.temporary_directory.name) / "custom.yml"
        self.config_path.write_text(CONFIGURATION)
        self.missing_path = Path(self.temporary_directory.name) / "missing.yml"
        self.default_path = ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH
        self.default_override = ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_OVERRIDE
        self.environment = mock.patch.dict(os.environ)
        self.environment.start()
        os.environ.pop(ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV, None)

    def tearDown(self):
        self.environment.stop()
        ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH = self.default_path
        ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_OVERRIDE = self.default_override
        self.temporary_directory.cleanup()

    @staticmethod
    def parse(arguments):
        namespace, _, _ = ConfigDrivenParser.parse_known_args(
            [MiddleWareParser], arguments=arguments, client=True
        )
        return namespace

    def test_default_configuration_unset_env(self):
        self.assertEqual(
            ConfigDrivenParser.get_default_configuration(),
            (ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH, False),
        )

    def test_default_configuration_empty_env_is_unset(self):
        os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = ""
        self.assertEqual(
            ConfigDrivenParser.get_default_configuration(),
            (ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH, False),
        )

    def test_default_configuration_from_env(self):
        os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = str(
            self.config_path
        )
        self.assertEqual(
            ConfigDrivenParser.get_default_configuration(), (self.config_path, True)
        )

    def test_set_default_configuration_overrides_env_and_keeps_it(self):
        os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = str(
            self.missing_path
        )
        ConfigDrivenParser.set_default_configuration(self.config_path)
        self.assertEqual(
            ConfigDrivenParser.get_default_configuration(), (self.config_path, False)
        )
        self.assertEqual(
            os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV],
            str(self.missing_path),
        )
        self.assertEqual(self.parse([]).tts_port, 60000)

    def test_env_configuration_applied_and_cli_wins(self):
        os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = str(
            self.config_path
        )
        namespace = self.parse([])
        self.assertEqual(namespace.config, self.config_path)
        self.assertEqual(namespace.tts_port, 60000)
        self.assertEqual(namespace.tts_addr, "10.0.0.5")

        namespace = self.parse(["--tts-port", "1234"])
        self.assertEqual(namespace.tts_port, 1234)
        self.assertEqual(namespace.tts_addr, "10.0.0.5")

    def test_explicit_configuration_wins_over_env(self):
        os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = str(
            self.missing_path
        )
        namespace = self.parse(["--config", str(self.config_path)])
        self.assertEqual(namespace.config, self.config_path)
        self.assertEqual(namespace.tts_port, 60000)

    def test_missing_default_configuration_is_ignored(self):
        ConfigDrivenParser.set_default_configuration(self.missing_path)
        namespace = self.parse([])
        self.assertEqual(namespace.config, self.missing_path)
        self.assertEqual(namespace.tts_port, 50050)

    def test_missing_explicit_configuration_is_error(self):
        with self.assertRaises(SystemExit):
            self.parse(["--config", str(self.missing_path)])

    def test_missing_env_configuration_is_error(self):
        os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = str(
            self.missing_path
        )
        with self.assertRaises(SystemExit):
            self.parse([])

    def test_pytest_config_flag_in_sys_argv_is_ignored(self):
        ConfigDrivenParser.set_default_configuration(self.missing_path)
        with mock.patch("sys.argv", ["pytest", "-c", "pytest.ini"]):
            self.assertEqual(self.parse([]).tts_port, 50050)
