import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from fprime_gds.common.testing_fw.pytest_integration import (
    explicit_pipeline_arguments,
)
from fprime_gds.executables.cli import (
    ConfigDrivenParser,
    MiddleWareParser,
    StandardPipelineParser,
)

CONFIGURATION = "command-line-options:\n  tts-port: 60000\n  tts-addr: 10.0.0.5\n"


def pytest_config(args, addopts=()):
    """Stand-in for the pytest config object as seen by the fixture"""
    return SimpleNamespace(
        getini=lambda name: list(addopts) if name == "addopts" else [],
        invocation_params=SimpleNamespace(args=tuple(args)),
    )


class TestExplicitPipelineArguments(unittest.TestCase):

    def setUp(self):
        self.pipeline_parser = StandardPipelineParser()
        self.environment = mock.patch.dict(os.environ)
        self.environment.start()
        os.environ.pop("PYTEST_ADDOPTS", None)

    def tearDown(self):
        self.environment.stop()

    def test_no_pipeline_flags_reproduces_nothing(self):
        config = pytest_config(["-v", "-c", "pytest.ini", "-k", "foo", "tests/"])
        self.assertEqual(
            explicit_pipeline_arguments(config, self.pipeline_parser), []
        )

    def test_only_explicit_pipeline_flags_reproduced(self):
        config = pytest_config(
            ["-v", "--tts-port", "1234", "--no-zmq", "--log-directly", "tests/"]
        )
        self.assertEqual(
            explicit_pipeline_arguments(config, self.pipeline_parser),
            ["--log-directly", "--no-zmq", "--tts-port", "1234"],
        )

    def test_addopts_and_environment_included(self):
        os.environ["PYTEST_ADDOPTS"] = "--tts-addr 10.1.1.1"
        config = pytest_config(["-v"], addopts=["--tts-port", "1234"])
        self.assertEqual(
            explicit_pipeline_arguments(config, self.pipeline_parser),
            ["--tts-port", "1234", "--tts-addr", "10.1.1.1"],
        )

    def test_configuration_applies_unless_flag_explicit(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            config_path = Path(temporary_directory) / "fprime-gds.yml"
            config_path.write_text(CONFIGURATION)
            os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV] = str(
                config_path
            )

            config = pytest_config(["-v"])
            namespace, _, _ = ConfigDrivenParser.parse_known_args(
                [MiddleWareParser],
                arguments=explicit_pipeline_arguments(config, self.pipeline_parser),
                client=True,
            )
            self.assertEqual(namespace.tts_port, 60000)
            self.assertEqual(namespace.tts_addr, "10.0.0.5")

            config = pytest_config(["-v", "--tts-port", "1234"])
            namespace, _, _ = ConfigDrivenParser.parse_known_args(
                [MiddleWareParser],
                arguments=explicit_pipeline_arguments(config, self.pipeline_parser),
                client=True,
            )
            self.assertEqual(namespace.tts_port, 1234)
            self.assertEqual(namespace.tts_addr, "10.0.0.5")
