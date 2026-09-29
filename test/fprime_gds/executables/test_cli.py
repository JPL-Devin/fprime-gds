"""Tests for configuration-file handling in fprime_gds.executables.cli"""

import argparse
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import yaml

from fprime_gds.executables import cli
from fprime_gds.executables.cli import (
    BINARY,
    CONFIGURATION,
    DICTIONARY,
    GUI,
    MIDDLEWARE,
    STANDARD_PIPELINE,
    Fragment,
    apply_configuration,
    flatten,
    handle_arguments,
    parse_args,
    reproduce_arguments,
    set_default_configuration,
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

    def apply(self, options):
        apply_configuration(self.parser, {"command-line-options": options})

    def test_scalars_are_type_converted(self):
        self.apply({"port": "6000", "name": 6000, "path": "some/where"})
        namespace = self.parser.parse_args(["--needed", "x"])
        self.assertEqual(namespace.port, 6000)
        self.assertEqual(namespace.name, "6000")
        self.assertEqual(namespace.path, Path("some/where"))

    def test_optional_value_takes_const_when_bare(self):
        parser = argparse.ArgumentParser()
        parser.add_argument("--output-unframed-data", nargs="?", const="unframed.log", default=None)
        apply_configuration(
            parser, {"command-line-options": {"output-unframed-data": None}}
        )
        self.assertEqual(parser.parse_args([]).output_unframed_data, "unframed.log")
        apply_configuration(
            parser, {"command-line-options": {"output-unframed-data": "other.log"}}
        )
        self.assertEqual(parser.parse_args([]).output_unframed_data, "other.log")

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

    def test_unknown_option_warns(self):
        with mock.patch("sys.stderr") as stderr:
            self.apply({"unknown-option": 1})
            self.assertTrue(stderr.write.called)


ORDER = []


def recording(name: str) -> Fragment:
    """Fragment recording the order handlers run in"""
    return Fragment(name, {(f"--{name.lower()}",): {"default": None}}, lambda args, **kwargs: ORDER.append(name))


FIRST, SECOND, THIRD = recording("First"), recording("Second"), recording("Third")


class TestComposition(unittest.TestCase):
    """Composition preserves declaration order and uses a repeated fragment once"""

    def setUp(self):
        ORDER.clear()

    def test_handlers_run_in_declaration_order(self):
        inner = Fragment("inner", parts=[THIRD, SECOND])
        handle_arguments([FIRST, inner, THIRD], argparse.Namespace())
        self.assertEqual(ORDER, ["First", "Third", "Second"])

    def test_repeated_fragments_are_kept_once(self):
        fragments = flatten([STANDARD_PIPELINE, DICTIONARY, Fragment("again", parts=[DICTIONARY])])
        self.assertEqual(sum(1 for fragment in fragments if fragment is DICTIONARY), 1)

    def test_parse_args_runs_handlers_once_in_order(self):
        namespace, _ = parse_args([SECOND, FIRST, SECOND], arguments=[])
        self.assertEqual(ORDER, ["Second", "First"])
        self.assertIsNone(namespace.first)

    def test_handler_error_exits_with_usage(self):
        failing = Fragment("failing", {("--fail",): {}}, lambda args, **kwargs: (_ for _ in ()).throw(ValueError("bad")))
        with mock.patch("sys.stderr") as stderr, self.assertRaises(SystemExit):
            parse_args([failing], arguments=[])
        self.assertIn("[ERROR] Failed to parse arguments: bad", "".join(str(c.args[0]) for c in stderr.write.call_args_list))


class TestReproduceArguments(unittest.TestCase):
    """A namespace reproduced as a command line parses back to the same values"""

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.fragments = [BINARY, GUI, MIDDLEWARE]

    def test_round_trip(self):
        cli_arguments = [
            "--no-app", "--gui", "none", "--gui-port", "6000", "--skip-browser-open",
            "--zmq-transport", "ipc:///tmp/a", "ipc:///tmp/b",
            "--application-arguments=-p", "--application-arguments=50000",
        ]
        namespace, _ = parse_args(self.fragments, arguments=cli_arguments)
        reproduced = reproduce_arguments([CONFIGURATION, *self.fragments], namespace)
        self.assertNotIn("--config", " ".join(reproduced))
        reloaded, _ = parse_args(self.fragments, arguments=reproduced)
        for key in ("noapp", "gui", "gui_port", "browser_auto_open", "zmq", "zmq_transport", "application_arguments"):
            self.assertEqual(getattr(reloaded, key), getattr(namespace, key), key)

    def test_configuration_file_read_is_handed_on(self):
        source = Path(self.tempdir.name) / "fprime-gds.yml"
        source.write_text("flask:\n  KEY: !PATH static/custom.js\ncommand-line-options:\n  gui-port: 6001\n  no-app:\n")
        namespace, _ = parse_args(self.fragments, arguments=["--config", str(source)])
        reproduced = reproduce_arguments([CONFIGURATION, *self.fragments], namespace)
        self.assertIn(f"--config={source}", reproduced)
        reloaded, _ = parse_args([GUI], arguments=reproduced[:1])
        self.assertEqual(reloaded.gui_port, "6001")
        self.assertEqual(reloaded.config_values["flask"]["KEY"], Path(self.tempdir.name).absolute() / "static/custom.js")

    def test_configured_list_items_are_not_reproduced_twice(self):
        source = Path(self.tempdir.name) / "fprime-gds.yml"
        source.write_text("command-line-options:\n  application-arguments: ['-p', '50000']\n")
        namespace, _ = parse_args(self.fragments, arguments=["--config", str(source), "--no-app", "--application-arguments=-x"])
        self.assertEqual(namespace.application_arguments, ["-p", "50000", "-x"])
        reproduced = reproduce_arguments([CONFIGURATION, *self.fragments], namespace)
        self.assertEqual([item for item in reproduced if item.startswith("--application-arguments")], ["--application-arguments=-x"])
        reloaded, _ = parse_args(self.fragments, arguments=reproduced)
        self.assertEqual(reloaded.application_arguments, ["-p", "50000", "-x"])

    def test_optional_value_flag_reproduces_bare_or_valued(self):
        namespace, _ = parse_args([cli.COMM_EXTRA], arguments=["--output-unframed-data"])
        self.assertEqual(reproduce_arguments([cli.COMM_EXTRA], namespace), ["--output-unframed-data"])
        namespace, _ = parse_args([cli.COMM_EXTRA], arguments=["--output-unframed-data", "-"])
        self.assertEqual(reproduce_arguments([cli.COMM_EXTRA], namespace), ["--output-unframed-data=-"])


class TestApplicationArguments(unittest.TestCase):
    """--application-arguments accepts dash-prefixed values from the configuration file"""

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)

    def parse(self, config, *cli_arguments):
        config_path = Path(self.tempdir.name) / "fprime-gds.yml"
        config_path.write_text(yaml.safe_dump({"command-line-options": config}))
        namespace, _ = parse_args(
            [BINARY, GUI, MIDDLEWARE], arguments=["--config", str(config_path), "--no-app", *cli_arguments]
        )
        return namespace, []

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


class ConfigurationStateTestCase(unittest.TestCase):
    """Snapshots/restores the module-level default configuration state so tests don't leak into each other"""

    def setUp(self):
        self._orig_default_path = cli._default_configuration
        self._orig_ignore_env = cli._ignore_configuration_env

    def tearDown(self):
        cli._default_configuration = self._orig_default_path
        cli._ignore_configuration_env = self._orig_ignore_env


class TestDefaultConfiguration(ConfigurationStateTestCase):
    """Default configuration resolution: FPRIME_GDS_CONFIG_PATH > built-in default, unless set_default_configuration"""

    def test_default_configuration_without_env_var(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(cli.CONFIGURATION_PATH_ENV, None)
            self.assertEqual(
                cli.default_configuration(),
                Path("fprime-gds.yml"),
            )

    def test_env_var_overrides_built_in_default(self):
        with mock.patch.dict(
            os.environ,
            {cli.CONFIGURATION_PATH_ENV: "/tmp/custom.yml"},
        ):
            self.assertEqual(
                cli.default_configuration(), Path("/tmp/custom.yml")
            )

    def test_empty_env_var_is_treated_as_unset(self):
        # Must not resolve to Path(""), i.e. the current working directory.
        with mock.patch.dict(
            os.environ, {cli.CONFIGURATION_PATH_ENV: ""}
        ):
            self.assertEqual(
                cli.default_configuration(),
                cli.DEFAULT_CONFIGURATION_PATH,
            )

    def test_set_default_configuration_wins_over_env_var(self):
        with mock.patch.dict(
            os.environ,
            {cli.CONFIGURATION_PATH_ENV: "/tmp/from-env.yml"},
        ):
            set_default_configuration(Path("/tmp/explicit.yml"))
            self.assertEqual(
                cli.default_configuration(),
                Path("/tmp/explicit.yml"),
            )
            # Must not mutate the environment (child processes still see the original variable).
            self.assertEqual(
                os.environ[cli.CONFIGURATION_PATH_ENV],
                "/tmp/from-env.yml",
            )

    def test_set_default_configuration_none_ignores_env_var(self):
        with mock.patch.dict(
            os.environ,
            {cli.CONFIGURATION_PATH_ENV: "/tmp/from-env.yml"},
        ):
            set_default_configuration(None)
            self.assertIsNone(cli.default_configuration())


class TestLoadConfiguration(ConfigurationStateTestCase):
    """The configuration fragment's explicit-configuration detection

    Must use the arguments given, not sys.argv (callers like the pytest fixture parse an argument list that differs from
    pytest's own sys.argv), and must agree with default_configuration() on whether set_default_configuration() has
    overridden the environment variable.
    """

    def _make_args(self, config_path):
        return argparse.Namespace(
            config=Path(config_path) if config_path is not None else None
        )

    def test_missing_config_not_explicit_is_ignored(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(cli.CONFIGURATION_PATH_ENV, None)
            args = self._make_args("does-not-exist.yml")
            result = handle_arguments([CONFIGURATION], args, arguments=["--foo", "bar"])
            self.assertEqual(result.config_values, {})

    def test_missing_config_explicit_via_arguments_raises(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(cli.CONFIGURATION_PATH_ENV, None)
            args = self._make_args("does-not-exist.yml")
            with self.assertRaises(ValueError):
                handle_arguments([CONFIGURATION], args, arguments=["--config", "does-not-exist.yml"])

    def test_missing_config_explicit_via_env_var_raises(self):
        # Must fail loudly rather than silently falling back to built-in defaults.
        with mock.patch.dict(
            os.environ,
            {cli.CONFIGURATION_PATH_ENV: "does-not-exist.yml"},
        ):
            args = self._make_args("does-not-exist.yml")
            with self.assertRaises(ValueError):
                handle_arguments([CONFIGURATION], args, arguments=["--foo", "bar"])

    def test_env_var_ignored_after_set_default_configuration_none(self):
        # A stale env var must not be treated as explicit (args.config is None here, so
        # args.config.exists() would crash).
        with mock.patch.dict(
            os.environ,
            {cli.CONFIGURATION_PATH_ENV: "/nonexistent.yml"},
        ):
            set_default_configuration(None)
            args = self._make_args(None)
            result = handle_arguments([CONFIGURATION], args, arguments=["--foo", "bar"])
            self.assertEqual(result.config_values, {})

    def test_env_var_ignored_after_set_default_configuration_override(self):
        with mock.patch.dict(
            os.environ,
            {cli.CONFIGURATION_PATH_ENV: "/nonexistent-env.yml"},
        ):
            with tempfile.TemporaryDirectory() as tmp_dir:
                override_path = Path(tmp_dir) / "override.yml"
                override_path.write_text("command-line-options:\n  logs: /tmp/logs\n")
                set_default_configuration(override_path)
                args = self._make_args(override_path)
                result = handle_arguments([CONFIGURATION], args, arguments=["--foo", "bar"])
        self.assertEqual(
            result.config_values, {"command-line-options": {"logs": "/tmp/logs"}}
        )

    def test_sys_argv_is_not_consulted(self):
        # A driving tool's own sys.argv (e.g. pytest's `-c pytest.ini`) must not be mistaken
        # for an explicit --config to this parser.
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop(cli.CONFIGURATION_PATH_ENV, None)
            args = self._make_args("does-not-exist.yml")
            with mock.patch("sys.argv", ["pytest", "-c", "pytest.ini"]):
                result = handle_arguments([CONFIGURATION], args, arguments=["--foo", "bar"])
            self.assertEqual(result.config_values, {})

    def test_existing_config_file_is_loaded(self):
        with tempfile.TemporaryDirectory() as tmp_dir:
            config_path = Path(tmp_dir) / "config.yml"
            config_path.write_text("command-line-options:\n  logs: /tmp/logs\n")
            args = self._make_args(str(config_path))
            result = handle_arguments([CONFIGURATION], args, arguments=["--config", str(config_path)])
            self.assertEqual(
                result.config_values, {"command-line-options": {"logs": "/tmp/logs"}}
            )


if __name__ == "__main__":
    unittest.main()
