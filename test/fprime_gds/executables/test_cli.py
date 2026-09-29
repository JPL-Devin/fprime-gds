"""Tests for configuration-file handling in fprime_gds.executables.cli"""

import argparse
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import yaml

from fprime_gds.executables.cli import (
    BinaryDeployment,
    CompositeParser,
    ConfigDrivenParser,
    DictionaryParser,
    GdsParser,
    MiddleWareParser,
    ParserBase,
    StandardPipelineParser,
)


class TestConfiguredDefaults(unittest.TestCase):
    """Configuration values become argparse defaults with the argument's type applied"""

    def setUp(self):
        self.parser = argparse.ArgumentParser()
        self.parser.add_argument("--port", type=int, default=50000)
        self.parser.add_argument("--name", type=str, default="fprime")
        self.parser.add_argument("--path", type=Path, default=None)
        self.parser.add_argument("--gui", choices=["none", "html"], default="html")
        self.parser.add_argument("--flag", action="store_true", default=False)
        self.parser.add_argument("--inverted", action="store_false", dest="normal", default=True)
        self.parser.add_argument("--pair", nargs=2, type=str, default=None)
        self.parser.add_argument("--many", action="extend", nargs="*", type=str, default=None)
        self.parser.add_argument("--needed", type=str, required=True)

    def apply(self, options, generated=False):
        config = {"command-line-options": options}
        if generated:
            config["generated"] = True
        ConfigDrivenParser.apply_configuration(self.parser, config)

    def test_scalars_are_type_converted(self):
        self.apply({"port": "6000", "name": 6000, "path": "some/where"})
        namespace = self.parser.parse_args(["--needed", "x"])
        self.assertEqual(namespace.port, 6000)
        self.assertEqual(namespace.name, "6000")
        self.assertEqual(namespace.path, Path("some/where"))

    def test_flags_take_none_true_or_false(self):
        self.apply({"flag": None, "inverted": True})
        namespace = self.parser.parse_args(["--needed", "x"])
        self.assertTrue(namespace.flag)
        self.assertFalse(namespace.normal)

        self.apply({"flag": False, "inverted": False})
        namespace = self.parser.parse_args(["--needed", "x"])
        self.assertFalse(namespace.flag)
        self.assertTrue(namespace.normal)

    def test_lists_for_multi_valued_options(self):
        self.apply({"pair": ["a", "b"], "many": ["-p", "50000"]})
        namespace = self.parser.parse_args(["--needed", "x"])
        self.assertEqual(namespace.pair, ["a", "b"])
        self.assertEqual(namespace.many, ["-p", "50000"])

    def test_scalar_for_extend_option_is_wrapped(self):
        self.apply({"many": "-p"})
        self.assertEqual(self.parser.parse_args(["--needed", "x"]).many, ["-p"])

    def test_configured_value_satisfies_required(self):
        self.apply({"needed": "from-config"})
        self.assertEqual(self.parser.parse_args([]).needed, "from-config")

    def test_command_line_overrides_configuration(self):
        self.apply({"port": 6000, "flag": True, "gui": "none"})
        namespace = self.parser.parse_args(["--needed", "x", "--port", "7000", "--gui", "html"])
        self.assertEqual(namespace.port, 7000)
        self.assertEqual(namespace.gui, "html")
        self.assertTrue(namespace.flag)

    def test_invalid_choice_is_rejected(self):
        with self.assertRaises(ValueError):
            self.apply({"gui": "qt"})

    def test_unknown_option_warns_unless_generated(self):
        with mock.patch("sys.stderr") as stderr:
            self.apply({"unknown-option": 1})
            self.assertTrue(stderr.write.called)
        with mock.patch("sys.stderr") as stderr:
            self.apply({"unknown-option": 1}, generated=True)
            self.assertFalse(stderr.write.called)


class Recording(ParserBase):
    """Fragment recording the order handlers run in"""

    ORDER = []
    DESCRIPTION = "Recording"

    def get_arguments(self):
        return {(f"--{self.__class__.__name__.lower()}",): {"default": None}}

    def handle_arguments(self, args, **kwargs):
        self.ORDER.append(self.__class__.__name__)
        return args


class First(Recording):
    pass


class Second(Recording):
    pass


class Third(Recording):
    pass


class TestCompositeParser(unittest.TestCase):
    """Composition preserves declaration order and removes repeated fragments"""

    def setUp(self):
        Recording.ORDER.clear()

    def test_handlers_run_in_declaration_order(self):
        inner = CompositeParser([Third, Second])
        composite = CompositeParser([First, inner, Third])
        composite.handle_arguments(argparse.Namespace())
        self.assertEqual(Recording.ORDER, ["First", "Third", "Second"])

    def test_repeated_fragments_are_kept_once(self):
        composite = CompositeParser(
            [StandardPipelineParser, DictionaryParser, CompositeParser([DictionaryParser])]
        )
        dictionary_parsers = [
            item for item in composite.constituents if isinstance(item, DictionaryParser)
        ]
        self.assertEqual(len(dictionary_parsers), 1)

    def test_parse_args_runs_handlers_once_in_order(self):
        namespace, _ = ParserBase.parse_args([Second, First, Second], arguments=[])
        self.assertEqual(Recording.ORDER, ["Second", "First"])
        self.assertIsNone(namespace.first)


class TestResolvedConfiguration(unittest.TestCase):
    """A parsed namespace written with write_configuration reloads to the same values"""

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.composite = CompositeParser([BinaryDeployment, GdsParser, MiddleWareParser])

    def test_round_trip(self):
        cli = [
            "--no-app",
            "--gui",
            "none",
            "--gui-port",
            "6000",
            "--zmq-transport",
            "ipc:///tmp/a",
            "ipc:///tmp/b",
            "--application-arguments=-p",
            "--application-arguments=50000",
        ]
        namespace, _ = ParserBase.parse_args([self.composite], arguments=cli)
        written = self.composite.write_configuration(
            namespace, Path(self.tempdir.name) / "resolved.yml"
        )
        self.assertTrue(yaml.safe_load(written.read_text())["generated"])

        reloaded, _ = ParserBase.parse_args(
            [self.composite], arguments=["--config", str(written)]
        )
        for key in ("app", "gui", "gui_port", "zmq", "zmq_transport", "application_arguments"):
            self.assertEqual(getattr(reloaded, key), getattr(namespace, key), key)

    def test_reload_with_a_subset_of_fragments_does_not_warn(self):
        namespace, _ = ParserBase.parse_args(
            [self.composite], arguments=["--no-app", "--gui", "none"]
        )
        written = self.composite.write_configuration(
            namespace, Path(self.tempdir.name) / "resolved.yml"
        )
        with mock.patch("sys.stderr") as stderr:
            reloaded, _ = ParserBase.parse_args(
                [MiddleWareParser], arguments=["--config", str(written)]
            )
            self.assertFalse(stderr.write.called)
        self.assertEqual(reloaded.zmq_transport, namespace.zmq_transport)


class TestApplicationArguments(unittest.TestCase):
    """--application-arguments accepts dash-prefixed values from the configuration file"""

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)

    def parse(self, config, *cli):
        config_path = Path(self.tempdir.name) / "fprime-gds.yml"
        config_path.write_text(yaml.safe_dump({"command-line-options": config}))
        namespace, _, remaining = ConfigDrivenParser.parse_known_args(
            [BinaryDeployment, GdsParser, MiddleWareParser],
            arguments=["--config", str(config_path), "--no-app", *cli],
        )
        return namespace, remaining

    def test_config_list_with_dash_values(self):
        expected = ["-p", "50000", "-a", "0.0.0.0", "-k", "sdls.key"]
        namespace, remaining = self.parse({"application-arguments": expected})
        self.assertEqual(namespace.application_arguments, expected)
        self.assertEqual(remaining, [])

    def test_config_list_with_dash_values_does_not_disturb_other_options(self):
        namespace, _ = self.parse(
            {"application-arguments": ["-p", "50000"], "gui-port": 6000, "gui": "none"}
        )
        self.assertEqual(namespace.application_arguments, ["-p", "50000"])
        self.assertEqual(namespace.gui_port, "6000")
        self.assertEqual(namespace.gui, "none")

    def test_unset_defaults_to_none(self):
        namespace, _ = self.parse({"gui": "none"})
        self.assertIsNone(namespace.application_arguments)

    def test_cli_plain_values(self):
        namespace, remaining = self.parse({}, "--application-arguments", "foo", "bar")
        self.assertEqual(namespace.application_arguments, ["foo", "bar"])
        self.assertEqual(remaining, [])

    def test_cli_equals_form_with_dash_values(self):
        namespace, remaining = self.parse(
            {},
            "--application-arguments=-p",
            "--application-arguments=50000",
            "--gui",
            "none",
        )
        self.assertEqual(namespace.application_arguments, ["-p", "50000"])
        self.assertEqual(remaining, [])

    def test_cli_extends_config(self):
        namespace, _ = self.parse(
            {"application-arguments": ["-p", "50000"]}, "--application-arguments=-k"
        )
        self.assertEqual(namespace.application_arguments, ["-p", "50000", "-k"])


class TestConfigDrivenParserDefaultConfiguration(unittest.TestCase):
    """Tests for ConfigDrivenParser's (global) default configuration resolution

    Covers get_default_configuration()/set_default_configuration() precedence (-c/--config >
    FPRIME_GDS_CONFIG_PATH > built-in default), and that set_default_configuration() does not
    mutate os.environ (so child processes still see the original variable).
    """

    def setUp(self):
        # Snapshot/restore ConfigDrivenParser's class-level state so tests don't leak into each other.
        self._orig_default_path = ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH
        self._orig_explicit = ConfigDrivenParser._DEFAULT_CONFIGURATION_EXPLICIT

    def tearDown(self):
        ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH = self._orig_default_path
        ConfigDrivenParser._DEFAULT_CONFIGURATION_EXPLICIT = self._orig_explicit

    def test_default_configuration_without_env_var(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV, None)
            self.assertEqual(
                ConfigDrivenParser.get_default_configuration(),
                Path("fprime-gds.yml"),
            )

    def test_env_var_overrides_built_in_default(self):
        with mock.patch.dict(
            os.environ,
            {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: "/tmp/custom.yml"},
        ):
            self.assertEqual(
                ConfigDrivenParser.get_default_configuration(), Path("/tmp/custom.yml")
            )

    def test_empty_env_var_is_treated_as_unset(self):
        # Must not resolve to Path(""), i.e. the current working directory.
        with mock.patch.dict(
            os.environ, {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: ""}
        ):
            self.assertEqual(
                ConfigDrivenParser.get_default_configuration(),
                ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH,
            )

    def test_set_default_configuration_wins_over_env_var(self):
        with mock.patch.dict(
            os.environ,
            {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: "/tmp/from-env.yml"},
        ):
            ConfigDrivenParser.set_default_configuration(Path("/tmp/explicit.yml"))
            self.assertEqual(
                ConfigDrivenParser.get_default_configuration(),
                Path("/tmp/explicit.yml"),
            )
            # Must not mutate the environment (child processes still see the original variable).
            self.assertEqual(
                os.environ[ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV],
                "/tmp/from-env.yml",
            )

    def test_set_default_configuration_none_ignores_env_var(self):
        with mock.patch.dict(
            os.environ,
            {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: "/tmp/from-env.yml"},
        ):
            ConfigDrivenParser.set_default_configuration(None)
            self.assertIsNone(ConfigDrivenParser.get_default_configuration())


class TestConfigDrivenParserHandleArguments(unittest.TestCase):
    """Tests for ConfigDrivenParser.handle_arguments()'s explicit-configuration detection

    Must use the `arguments` passed to the parser, not sys.argv (callers like the pytest fixture
    in pytest_integration.py parse an argument list that differs from pytest's own sys.argv), and
    must agree with get_default_configuration() on whether set_default_configuration() has
    overridden the environment variable.
    """

    def setUp(self):
        self._orig_default_path = ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH
        self._orig_explicit = ConfigDrivenParser._DEFAULT_CONFIGURATION_EXPLICIT

    def tearDown(self):
        ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH = self._orig_default_path
        ConfigDrivenParser._DEFAULT_CONFIGURATION_EXPLICIT = self._orig_explicit

    def _make_args(self, config_path):
        return argparse.Namespace(
            config=Path(config_path) if config_path is not None else None
        )

    def test_missing_config_not_explicit_is_ignored(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV, None)
            args = self._make_args("does-not-exist.yml")
            result = ConfigDrivenParser().handle_arguments(
                args, arguments=["--foo", "bar"]
            )
            self.assertEqual(result.config_values, {})

    def test_missing_config_explicit_via_arguments_raises(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV, None)
            args = self._make_args("does-not-exist.yml")
            with self.assertRaises(ValueError):
                ConfigDrivenParser().handle_arguments(
                    args, arguments=["--config", "does-not-exist.yml"]
                )

    def test_missing_config_explicit_via_env_var_raises(self):
        # Must fail loudly rather than silently falling back to built-in defaults.
        with mock.patch.dict(
            os.environ,
            {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: "does-not-exist.yml"},
        ):
            args = self._make_args("does-not-exist.yml")
            with self.assertRaises(ValueError):
                ConfigDrivenParser().handle_arguments(args, arguments=["--foo", "bar"])

    def test_env_var_ignored_after_set_default_configuration_none(self):
        # A stale env var must not be treated as explicit (args.config is None here, so
        # args.config.exists() would crash).
        with mock.patch.dict(
            os.environ,
            {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: "/nonexistent.yml"},
        ):
            ConfigDrivenParser.set_default_configuration(None)
            args = self._make_args(None)
            result = ConfigDrivenParser().handle_arguments(
                args, arguments=["--foo", "bar"]
            )
            self.assertEqual(result.config_values, {})

    def test_env_var_ignored_after_set_default_configuration_override(self):
        with mock.patch.dict(
            os.environ,
            {ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV: "/nonexistent-env.yml"},
        ):
            with tempfile.TemporaryDirectory() as tmp_dir:
                override_path = Path(tmp_dir) / "override.yml"
                override_path.write_text("command-line-options:\n  logs: /tmp/logs\n")
                ConfigDrivenParser.set_default_configuration(override_path)
                args = self._make_args(override_path)
                result = ConfigDrivenParser().handle_arguments(
                    args, arguments=["--foo", "bar"]
                )
        self.assertEqual(
            result.config_values, {"command-line-options": {"logs": "/tmp/logs"}}
        )

    def test_sys_argv_is_not_consulted(self):
        # A driving tool's own sys.argv (e.g. pytest's `-c pytest.ini`) must not be mistaken
        # for an explicit --config to this parser.
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(ConfigDrivenParser.DEFAULT_CONFIGURATION_PATH_ENV, None)
            args = self._make_args("does-not-exist.yml")
            with mock.patch("sys.argv", ["pytest", "-c", "pytest.ini"]):
                result = ConfigDrivenParser().handle_arguments(
                    args, arguments=["--foo", "bar"]
                )
            self.assertEqual(result.config_values, {})

    def test_existing_config_file_is_loaded(self):
        with tempfile.TemporaryDirectory() as tmp_dir:
            config_path = Path(tmp_dir) / "config.yml"
            config_path.write_text("command-line-options:\n  logs: /tmp/logs\n")
            args = self._make_args(str(config_path))
            result = ConfigDrivenParser().handle_arguments(
                args, arguments=["--config", str(config_path)]
            )
            self.assertEqual(
                result.config_values, {"command-line-options": {"logs": "/tmp/logs"}}
            )


if __name__ == "__main__":
    unittest.main()
