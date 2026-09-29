"""
cli.py:

Command line handling for the F prime GDS tools. Tools assemble their command line from small, reusable argument
fragments (subclasses of `ParserBase`), each of which declares its arguments as data and optionally post-processes the
parsed values. `ParserBase.parse_args` composes those fragments into a single `argparse` parser, layers in values from
the fprime-gds configuration file, and runs the fragments' post-processing in declaration order.

Precedence of an option's value is: command line > configuration file > declared default.

@author mstarch
"""

import argparse
import datetime
import errno
import functools
import getpass
import os
import platform
import re
import sys
from importlib.metadata import version

import yaml

from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

# Required to set the checksum as a module variable
import fprime_gds.common.logger
from fprime_gds.common.communication.adapters.ip import check_port
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.pipeline.standard import StandardPipeline
from fprime_gds.common.transport import ThreadedTCPSocketClient
from fprime_gds.common.utils.config_manager import ConfigManager
from fprime_gds.executables.utils import find_app, find_dict, get_artifacts_root
from fprime_gds.plugin.definitions import PluginType
from fprime_gds.plugin.system import Plugins, PluginsNotLoadedException
from fprime_gds.common.zmq_transport import ZmqClient

GUIS = ["none", "html"]

ArgumentSpecification = Dict[Tuple[str, ...], Dict[str, Any]]

FLAG_ACTIONS = ("store_true", "store_false", "store_const")
LIST_ACTIONS = ("append", "extend")


def _long_flag(flags: Iterable[str]) -> str:
    """Best flag for an argument: the first --long flag, else the first flag"""
    flags = list(flags)
    return ([flag for flag in flags if flag.startswith("--")] + flags)[0]


def _destination(flags: Iterable[str], argparse_inputs: Dict[str, Any]) -> str:
    """Namespace member an argument specification is stored into (mirrors argparse's dest derivation)"""
    return argparse_inputs.get("dest", re.sub(r"^-+", "", _long_flag(flags)).replace("-", "_"))


class ParserBase(ABC):
    """Base of all command line fragments

    A fragment declares its arguments through `get_arguments` and may post-process the parsed namespace through
    `handle_arguments`. Fragments are composed by `ParserBase.parse_args` (or a `CompositeParser`); post-processing runs
    in the order fragments are listed, so list producers (e.g. `DictionaryParser`) before consumers.
    """

    DESCRIPTION: Optional[str] = None

    @property
    def description(self) -> str:
        """Return parser description"""
        return self.DESCRIPTION if self.DESCRIPTION is not None else "Unknown command line parser"

    @abstractmethod
    def get_arguments(self) -> ArgumentSpecification:
        """Return the arguments declared by this fragment

        Returns:
            dictionary of flag tuples (e.g. ("-d", "--deployment")) to keyword arguments for `argparse.add_argument`
        """

    def handle_arguments(self, args: argparse.Namespace, **kwargs) -> argparse.Namespace:
        """Post-process the parsed namespace, returning it

        Override to validate or derive values. Raise ValueError to report a usage error. `kwargs` carry tool-wide
        flags, e.g. `client=True` when the tool connects to a running GDS rather than hosting it.
        """
        return args

    def get_parser(self) -> argparse.ArgumentParser:
        """Return a stand-alone argparse parser for this fragment's arguments"""
        parser = argparse.ArgumentParser(
            description=self.description,
            add_help=True,
            formatter_class=argparse.ArgumentDefaultsHelpFormatter,
        )
        self.fill_parser(parser)
        return parser

    @staticmethod
    def safe_add_argument(parser, *flags, **keywords):
        """Add an argument, ignoring the argparse error raised when the flag already exists

        Used when merging fragments and plugins so that two contributors declaring the same flag do not crash the tool.
        """
        try:
            parser.add_argument(*flags, **keywords)
        except argparse.ArgumentError:
            pass

    @classmethod
    def add_arguments_from_specification(cls, parser, arguments: ArgumentSpecification):
        """Add every argument of a specification to parser (or argument group), tolerating duplicates"""
        for flags, keywords in arguments.items():
            cls.safe_add_argument(parser, *flags, **keywords)

    def fill_parser(self, parser):
        """Add this fragment's arguments to an argparse parser, as an argument group titled with the description"""
        group = parser.add_argument_group(title=self.description)
        self.add_arguments_from_specification(group, self.get_arguments())

    def resolved_options(self, args_ns: argparse.Namespace) -> Dict[str, Any]:
        """Values of this fragment's arguments as configuration-file options

        Returns a dictionary keyed by long flag name without the leading dashes (the form used under
        `command-line-options` in a configuration file) whose values reload to the same namespace values. Flag
        arguments (store_true/store_false/store_const) become booleans, None values are omitted, and paths become
        strings. Feed the result to `write_configuration` to hand a fully-resolved command line to a child process.
        """
        options = {}
        for flags, argparse_inputs in self.get_arguments().items():
            action = argparse_inputs.get("action", "store")
            if action in ("help", "version"):
                continue
            value = getattr(args_ns, _destination(flags, argparse_inputs), None)
            if value is None:
                continue
            if action in FLAG_ACTIONS:
                value = value == argparse_inputs.get("const", action == "store_true")
            elif isinstance(value, (list, tuple)):
                value = [str(item) if isinstance(item, Path) else item for item in value]
            elif isinstance(value, Path):
                value = str(value)
            options[_long_flag(flags).lstrip("-")] = value
        return options

    def write_configuration(self, args_ns: argparse.Namespace, path: Path) -> Path:
        """Write a configuration file reproducing the values of this fragment's arguments

        The file is a complete fprime-gds configuration: any non-argument sections of the configuration the tool
        itself loaded (e.g. `flask:`) are carried over, and `command-line-options` holds `resolved_options`. It is
        marked `generated: true` so tools reading it do not warn about options they do not support.
        """
        loaded = dict(getattr(args_ns, "config_values", None) or {})
        loaded["command-line-options"] = self.resolved_options(args_ns)
        loaded["generated"] = True
        path = Path(path)
        with open(path, "w") as file_handle:
            yaml.safe_dump(loaded, file_handle, default_flow_style=False)
        return path

    def reproduce_cli_args(self, args_ns: argparse.Namespace) -> List[str]:
        """Reproduce a command line from a namespace for this fragment's arguments

        Prefer `write_configuration` plus `--config` when launching a child process; this exists for callers that must
        build an argument list.
        """
        arguments = []
        for flags, argparse_inputs in self.get_arguments().items():
            action = argparse_inputs.get("action", "store")
            if action in ("help", "version"):
                continue
            flag = _long_flag(flags)
            value = getattr(args_ns, _destination(flags, argparse_inputs), None)
            if action in FLAG_ACTIONS:
                if value == argparse_inputs.get("const", action == "store_true"):
                    arguments.append(flag)
            elif action == "count":
                arguments.extend([flag] * int(value or 0))
            elif value is None:
                continue
            elif action in LIST_ACTIONS:
                values = value if isinstance(value, (list, tuple)) else [value]
                arguments.extend(f"{flag}={item}" for item in values)
            else:
                values = value if isinstance(value, (list, tuple)) else [value]
                arguments.extend([flag] + [str(item) for item in values])
        return arguments

    @classmethod
    def parse_known_args(
        cls,
        parser_classes,
        description="No tool description provided",
        arguments=None,
        **kwargs,
    ):
        """Parse and post-process arguments, tolerating unknown arguments

        See `parse_args`. Returns (namespace, parser, unknown arguments).
        """
        return cls._parse_args(parser_classes, description, arguments, use_parse_known=True, **kwargs)

    @classmethod
    def parse_args(
        cls,
        parser_classes,
        description="No tool description provided",
        arguments=None,
        **kwargs,
    ):
        """Parse and post-process a tool's command line

        Composes the supplied fragments (classes or instances) and the configuration fragment into one parser, applies
        the configuration file's `command-line-options` as defaults, parses `arguments` (the command line when None),
        then runs each fragment's `handle_arguments` in order. Errors raised from handlers print the usage and the error,
        then exit the process.

        Args:
            parser_classes: ParserBase subclasses or instances to compose
            description: tool description shown in help
            arguments: arguments to parse; None uses sys.argv[1:]
            **kwargs: passed to every fragment's handle_arguments (e.g. client=True)
        Returns: (namespace, argparse parser)
        """
        ns, parser, _ = cls._parse_args(parser_classes, description, arguments, **kwargs)
        return ns, parser

    @staticmethod
    def _parse_args(parser_classes, description, arguments, use_parse_known=False, **kwargs):
        """Parsing flow shared by parse_args and parse_known_args"""
        arguments = sys.argv[1:] if arguments is None else list(arguments)
        composite = CompositeParser([ConfigDrivenParser, *parser_classes], description)
        parser = composite.get_parser()
        config_parser = composite.constituent(ConfigDrivenParser)
        try:
            _, config_values = config_parser.load(arguments)
            ConfigDrivenParser.apply_configuration(parser, config_values)
            if use_parse_known:
                args_ns, unknowns = parser.parse_known_args(arguments)
            else:
                args_ns, unknowns = parser.parse_args(arguments), []
            args_ns = composite.handle_arguments(args_ns, **kwargs)
        except Exception as exc:
            parser.print_usage(sys.stderr)
            print(f"[ERROR] Failed to parse arguments: {exc}", file=sys.stderr)
            sys.exit(-1)
        return args_ns, parser, unknowns


class ConfigDrivenParser(ParserBase):
    """Configuration file fragment

    Adds -c/--config and -v/--version, loads the YAML configuration file, and exposes it as `args.config_values`. The
    file is resolved, in order of precedence: -c/--config, the FPRIME_GDS_CONFIG_PATH environment variable, then
    `fprime-gds.yml` in the working directory. A file selected explicitly must exist; the implicit default is skipped
    when absent. `set_default_configuration` replaces the implicit default and disables the environment variable.

    Values under `command-line-options` (keyed by long flag name without dashes) become the defaults of the matching
    arguments, so the command line still wins. Flag options (e.g. `no-app:`) may be given without a value, or with a
    boolean. Options unknown to the tool are ignored with a warning unless the file is marked `generated: true`.
    """

    DESCRIPTION = "Configuration options"
    DEFAULT_CONFIGURATION_PATH = Path("fprime-gds.yml")
    DEFAULT_CONFIGURATION_PATH_ENV = "FPRIME_GDS_CONFIG_PATH"
    _DEFAULT_CONFIGURATION_EXPLICIT = False

    def __init__(self):
        self._loaded: Optional[Tuple[Optional[Path], Dict[str, Any]]] = None

    @classmethod
    def set_default_configuration(cls, path: Optional[Path]):
        """Set the implicit configuration path (None disables it) and ignore the environment variable"""
        cls.DEFAULT_CONFIGURATION_PATH = path
        cls._DEFAULT_CONFIGURATION_EXPLICIT = True

    @classmethod
    def _env_configuration_path(cls) -> Optional[Path]:
        """Path from the environment variable, or None when unset, empty, or overridden"""
        if cls._DEFAULT_CONFIGURATION_EXPLICIT:
            return None
        env_path = os.environ.get(cls.DEFAULT_CONFIGURATION_PATH_ENV)
        return Path(env_path) if env_path else None

    @classmethod
    def get_default_configuration(cls) -> Optional[Path]:
        """Configuration path used when -c/--config is not supplied"""
        return cls._env_configuration_path() or cls.DEFAULT_CONFIGURATION_PATH

    @classmethod
    def resolve_configuration(cls, arguments: List[str]) -> Tuple[Optional[Path], bool]:
        """Determine the configuration file for a command line

        Returns:
            (path or None, whether the path was explicitly requested via --config or the environment)
        """
        pre_parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
        pre_parser.add_argument("-c", "--config", dest="config", type=Path, default=None)
        pre_parsed, _ = pre_parser.parse_known_args(arguments)
        if pre_parsed.config is not None:
            return pre_parsed.config, True
        env_path = cls._env_configuration_path()
        if env_path is not None:
            return env_path, True
        return cls.DEFAULT_CONFIGURATION_PATH, False

    @classmethod
    def read_configuration(cls, path: Optional[Path], explicit: bool) -> Dict[str, Any]:
        """Read a configuration file, returning {} when an implicit file is absent

        Raises ValueError when an explicitly selected file is missing or malformed.
        """
        if path is None:
            return {}
        path = Path(path)
        if not path.exists():
            if explicit:
                raise ValueError(f"Specified configuration file '{path}' does not exist")
            return {}
        print(f"[INFO] Reading command-line configuration from: {path}")
        relative_base = path.parent.absolute()

        class Loader(yaml.SafeLoader):
            """Loader with a !PATH tag resolving paths relative to the configuration file"""

        Loader.add_constructor("!PATH", lambda loader, node: relative_base / loader.construct_scalar(node))
        try:
            with open(path, "r") as file_handle:
                loaded = yaml.load(file_handle, Loader=Loader)
        except Exception as exc:
            raise ValueError(f"Malformed configuration {path}: {exc}")
        if loaded is None:
            return {}
        if not isinstance(loaded, dict):
            raise ValueError(f"Malformed configuration {path}: expected a mapping at the top level")
        return loaded

    def load(self, arguments: List[str]) -> Tuple[Optional[Path], Dict[str, Any]]:
        """Resolve and read the configuration for a command line, once per instance"""
        if self._loaded is None:
            path, explicit = self.resolve_configuration(arguments)
            values = self.read_configuration(path, explicit)
            self._loaded = (path, values)
        return self._loaded

    @staticmethod
    def configured_default(action: argparse.Action, value: Any) -> Any:
        """Convert a configuration value into a default for an argparse action

        Flag actions (nargs == 0) take their constant when the value is None or True, and the opposite when False.
        Other actions apply the action's `type` to scalars, or to each item of a list for multi-valued actions.
        """
        if action.nargs == 0:
            if value is None or value is True:
                return action.const
            return (not action.const) if isinstance(action.const, bool) else action.default
        convert = action.type if callable(action.type) else (lambda item: item)

        def convert_item(item):
            return item if item is None else convert(str(item))

        multi_valued = action.nargs not in (None, argparse.OPTIONAL) or isinstance(
            action, (argparse._AppendAction, argparse._ExtendAction)
        )
        if multi_valued:
            items = value if isinstance(value, (list, tuple)) else [value]
            return [convert_item(item) for item in items]
        converted = convert_item(value)
        if action.choices is not None and converted not in action.choices:
            choices = ", ".join(str(choice) for choice in action.choices)
            raise ValueError(f"configured value '{value}' for '{_long_flag(action.option_strings)}' not in: {choices}")
        return converted

    @classmethod
    def apply_configuration(cls, parser: argparse.ArgumentParser, config_values: Dict[str, Any]):
        """Apply a configuration's `command-line-options` as the defaults of the parser's arguments

        Arguments given a configured value are no longer required. Options not declared by the parser are dropped
        with a warning, unless the configuration is marked `generated: true`.
        """
        options = config_values.get("command-line-options") or {}
        actions = {flag: action for action in parser._actions for flag in action.option_strings}
        for option, value in options.items():
            action = actions.get(f"--{option}")
            if action is None or isinstance(action, (argparse._HelpAction, argparse._VersionAction)):
                if not config_values.get("generated", False):
                    print(
                        f"[WARNING] Ignoring configured option '{option}' not supported by this tool",
                        file=sys.stderr,
                    )
                continue
            action.default = cls.configured_default(action, value)
            action.required = False

    def get_arguments(self) -> ArgumentSpecification:
        """Arguments needed for config processing"""
        return {
            ("-c", "--config"): {
                "dest": "config",
                "required": False,
                "default": self.get_default_configuration(),
                "type": Path,
                "help": "Argument configuration file path. [default: %(default)s]",
            },
            ("-v", "--version"): {
                "action": "version",
                "version": version("fprime_gds"),
            },
        }

    def handle_arguments(self, args, **kwargs):
        """Expose the loaded configuration as args.config (path) and args.config_values (dictionary)

        The configuration is loaded once per instance, by `ParserBase.parse_args` before parsing. When called directly,
        the command line to resolve `--config` from may be supplied as `arguments`; sys.argv is never consulted.
        """
        args.config, args.config_values = self.load(kwargs.get("arguments", []))
        return args


class DetectionParser(ParserBase):
    """Fragment locating the deployment (build output) directory, detecting it from settings.ini when not given"""

    def get_arguments(self) -> ArgumentSpecification:
        """Arguments needed for root processing"""
        return {
            ("-d", "--deployment"): {
                "dest": "deployment",
                "action": "store",
                "required": False,
                "type": str,
                "help": "Deployment installation/build output directory. [default: install_dest field in settings.ini]",
            }
        }

    def handle_arguments(self, args, **kwargs):
        """Handle the root, detecting it if necessary"""
        if args.deployment:
            args.deployment = Path(args.deployment)
            return args
        detected_toolchain = get_artifacts_root() / platform.system()
        if not detected_toolchain.exists():
            msg = f"{detected_toolchain} does not exist. Make sure to build."
            raise Exception(msg)
        likely_deployment = detected_toolchain / Path.cwd().name
        # Check if the deployment exists
        if likely_deployment.exists():
            args.deployment = likely_deployment
            return args
        child_directories = [child for child in detected_toolchain.iterdir() if child.is_dir()]
        if not child_directories:
            msg = f"No deployments found in {detected_toolchain}. Specify deployment with: --deployment"
            raise Exception(msg)
        # Works for the old structure where the bin, lib, and dict directories live immediately under the platform
        elif len(child_directories) == 3 and set([path.name for path in child_directories]) == {"bin", "lib", "dict"}:
            args.deployment = detected_toolchain
            return args
        elif len(child_directories) > 1:
            msg = f"Multiple deployments found in {detected_toolchain}. Choose using: --deployment"
            raise Exception(msg)
        args.deployment = child_directories[0]
        return args


class BareArgumentParser(ParserBase):
    """Fragment built from a raw argument specification (the form plugins return from `get_arguments`)

    `checking_function`, when supplied, is called as `checking_function(**values)` with the parsed value of each
    argument and should raise ValueError to reject them.
    """

    def __init__(self, specification: ArgumentSpecification, checking_function=None):
        """Initialize this parser with the provided specification"""
        self.specification = specification
        self.checking_function = checking_function

    def get_arguments(self) -> ArgumentSpecification:
        """Raw specification is returned immediately"""
        return self.specification

    def handle_arguments(self, args, **kwargs):
        """Handle argument calls checking function to validate"""
        if self.checking_function is not None:
            self.checking_function(**self.extract_arguments(args))
        return args

    def extract_arguments(self, args) -> Dict[str, Any]:
        """Extract this specification's values from the namespace as a {destination: value} dictionary"""
        return {
            _destination(flags, inputs): getattr(args, _destination(flags, inputs))
            for flags, inputs in self.specification.items()
        }


class PluginArgumentParser(ParserBase):
    """Fragment sourcing arguments from the plugin system

    For every plugin category this adds the category's arguments (a `--<category>-selection` flag for SELECTION
    categories) and each plugin's own `get_arguments` (plus `--disable-<name>` for FEATURE plugins). Handling validates
    each selected/enabled plugin's arguments via `check_arguments` and registers a constructor-bound class with the
    plugin system, ready for zero-argument construction.
    """

    DESCRIPTION = "Plugin options"
    # Defaults:
    FPRIME_CHOICES = {
        "framing": "space-packet-space-data-link",
        "communication": "ip",
    }

    def __init__(self, plugin_system: Plugins = None):
        """Initialize with the supplied plugin system, defaulting to the system singleton"""
        self.plugin_system = plugin_system if plugin_system else Plugins.system()
        self._plugin_map = {
            category: list(self.plugin_system.get_plugins(category))
            for category in self.plugin_system.get_categories()
        }

    @staticmethod
    def disable_flag_destination(plugin) -> str:
        """Namespace member of a FEATURE plugin's --disable-<name> flag"""
        return f"disable_{plugin.get_name()}".lower().replace("-", "_")

    def get_plugin_arguments(self, plugin) -> ArgumentSpecification:
        """Arguments of one plugin, including the disable flag of FEATURE plugins"""
        arguments: ArgumentSpecification = {}
        if plugin.type == PluginType.FEATURE:
            arguments[(f"--disable-{plugin.get_name()}",)] = {
                "action": "store_true",
                "default": False,
                "dest": self.disable_flag_destination(plugin),
                "help": f"Disable the {plugin.category} plugin '{plugin.get_name()}'",
            }
        arguments.update(plugin.get_arguments())
        return arguments

    def get_category_arguments(self, category) -> ArgumentSpecification:
        """Category-level arguments: a selection flag for SELECTION categories"""
        plugins = self._plugin_map[category]
        if self.plugin_system.get_category_plugin_type(category) != PluginType.SELECTION:
            return {}
        return {
            (f"--{category}-selection",): {
                "choices": [plugin.get_name() for plugin in plugins],
                "help": f"Select {category} implementer.",
                "default": self.FPRIME_CHOICES.get(category, plugins[0].get_name()),
            }
        }

    def get_arguments(self) -> ArgumentSpecification:
        """All category and plugin arguments"""
        arguments: ArgumentSpecification = {}
        for category, plugins in self._plugin_map.items():
            arguments.update(self.get_category_arguments(category))
            for plugin in plugins:
                arguments.update(self.get_plugin_arguments(plugin))
        return arguments

    def fill_parser(self, parser):
        """Add one argument group per category and per plugin"""
        for category, plugins in self._plugin_map.items():
            group = parser.add_argument_group(title=f"{category.title()} Plugin Options")
            self.add_arguments_from_specification(group, self.get_category_arguments(category))
            for plugin in plugins:
                group = parser.add_argument_group(title=f"{category.title()} Plugin '{plugin.get_name()}' Options")
                self.add_arguments_from_specification(group, self.get_plugin_arguments(plugin))

    def bind_plugin(self, plugin, args):
        """Validate a plugin's arguments and register its constructor-bound class with the plugin system"""
        specification = BareArgumentParser(plugin.get_arguments(), plugin.check_arguments)
        specification.handle_arguments(args)
        bound = functools.partial(plugin.get_implementor(), **specification.extract_arguments(args))
        self.plugin_system.add_bound_class(plugin.category, bound)

    def handle_arguments(self, args, **kwargs):
        """Bind the selected plugin of each SELECTION category and every enabled plugin of each FEATURE category"""
        for category, plugins in self._plugin_map.items():
            plugin_type = self.plugin_system.get_category_plugin_type(category)
            self.plugin_system.start_loading(category)
            if plugin_type == PluginType.SELECTION:
                try:
                    self.plugin_system.get_selected_class(category)
                    continue  # Already bound (e.g. by an earlier parse)
                except PluginsNotLoadedException:
                    pass
                selection = getattr(args, f"{category}_selection")
                matching = [plugin for plugin in plugins if plugin.get_name() == selection]
                assert len(matching) == 1, "Plugin selection system failed"
                self.bind_plugin(matching[0], args)
            else:
                for plugin in plugins:
                    if not getattr(args, self.disable_flag_destination(plugin), False):
                        self.bind_plugin(plugin, args)
        return args


class CompositeParser(ParserBase):
    """Composition of fragments into one fragment

    Constituents may be classes (constructed without arguments) or instances; nested composites are flattened.
    Declaration order is preserved and is the order `handle_arguments` runs in. A fragment appearing more than once
    (same class declaring the same flags, e.g. `DictionaryParser` reached through two composites) is kept once.
    """

    def __init__(self, constituents, description=None):
        """Construct this parser by instantiating and flattening the constituents"""
        self.given = description
        self.constituent_parsers: List[ParserBase] = []
        seen = set()
        for constituent in constituents:
            constructed = constituent() if isinstance(constituent, type) else constituent
            assert isinstance(constructed, ParserBase), f"{constructed.__class__.__name__} not a ParserBase child"
            items = constructed.constituents if isinstance(constructed, CompositeParser) else [constructed]
            for item in items:
                key = (type(item), frozenset(item.get_arguments().keys()))
                if key not in seen:
                    seen.add(key)
                    self.constituent_parsers.append(item)

    @property
    def constituents(self) -> List[ParserBase]:
        """Flattened constituent fragments, in order"""
        return self.constituent_parsers

    def constituent(self, parser_class):
        """First constituent that is an instance of parser_class, or None"""
        return next((item for item in self.constituents if isinstance(item, parser_class)), None)

    @property
    def description(self) -> str:
        """Return parser description"""
        return self.given if self.given else ",".join(item.description for item in self.constituents)

    def fill_parser(self, parser):
        """Fill the parser from each constituent, in declaration order"""
        for constituent in self.constituents:
            constituent.fill_parser(parser)

    def get_arguments(self) -> ArgumentSpecification:
        """Get the argument from all constituents"""
        arguments = {}
        for constituent in self.constituents:
            arguments.update(constituent.get_arguments())
        return arguments

    def handle_arguments(self, args, **kwargs):
        """Process all constituent arguments in order"""
        for constituent in self.constituents:
            args = constituent.handle_arguments(args, **kwargs)
        return args


class CommExtraParser(ParserBase):
    """Parses extra communication arguments"""

    DESCRIPTION = "Communications options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Get arguments for the comm-layer parser"""
        com_arguments = {
            ("--output-unframed-data",): {
                "dest": "output_unframed_data",
                "action": "store",
                "nargs": "?",
                "help": "Log unframed data to supplied file relative to log directory. Use '-' for standard out.",
                "default": None,
                "const": "unframed.log",
                "required": False,
            },
        }
        return com_arguments



class LogDeployParser(ParserBase):
    """
    A parser that handles log files by reading in a '--logs' directory or a '--deploy' directory to put the logs into
    as a default. This is useful as a parsing fragment for any application that produces log files and needs these logs
    to end up in the proper place.
    """

    DESCRIPTION = "Logging options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Return arguments to parse logging options"""
        return {
            ("-l", "--logs"): {
                "dest": "logs",
                "action": "store",
                "default": os.path.join(os.getcwd(), "logs"),
                "type": str,
                "help": "Logging directory. Created if non-existent. [default: %(default)s]",
            },
            ("--log-directly",): {
                "dest": "log_directly",
                "action": "store_true",
                "default": False,
                "help": "Logging directory is used directly, no extra dated directories created.",
            },
            ("--log-to-stdout",): {
                "action": "store_true",
                "default": False,
                "help": "Log to standard out along with log output files",
            },
            ("--log-level-gds",): {
                "action": "store",
                "dest": "log_level_gds",
                "choices": ["DEBUG", "INFO", "WARNING", "ERROR"],
                "default": "INFO",
                "help": "Set the logging level of GDS processes [default: %(default)s]",
            },
            ("--disable-data-logging",): {
                "action": "store_true",
                "default": False,
                "help": "Disable logging of each data item",
            },
            ("--log-prefix",): {
                "dest": "log_prefix",
                "action": "store",
                "default": None,
                "type": str,
                "help": "Prefix for log directory names (e.g. 'fprime-gds-<timestamp>'). "
                "Auto-detected from tool name when not specified. Use '' to disable. [default: auto-detect]",
            },
        }

    def handle_arguments(self, args, **kwargs):
        """
        Read the arguments specified in this parser and validate the expected inputs.

        :param args: parsed arguments as namespace
        :return: args namespace
        """
        # Get logging dir
        if not args.log_directly:
            # Auto-detect prefix from tool name if not explicitly provided
            if args.log_prefix is None:
                tool_name = os.path.basename(sys.argv[0])
                if tool_name.endswith(".py"):
                    tool_name = tool_name[:-3]
                args.log_prefix = tool_name

            timestamp = datetime.datetime.now().strftime("%Y_%m_%d-%H_%M_%S")
            dir_name = (
                f"{args.log_prefix}-{timestamp}" if args.log_prefix else timestamp
            )
            args.logs = os.path.abspath(os.path.join(args.logs, dir_name))
            # A dated directory has been set, all log handling must now be direct
            args.log_directly = True

        # Make sure directory exists
        try:
            os.makedirs(args.logs, exist_ok=True)
        except OSError as osexc:
            if osexc.errno != errno.EEXIST:
                raise
        # Setup the basic python logging
        fprime_gds.common.logger.configure_py_log(
            args.logs, mirror_to_stdout=args.log_to_stdout, log_level=args.log_level_gds
        )
        return args


class MiddleWareParser(ParserBase):
    """
    Middleware (ThreadedTcpServer, ZMQ) interface that looks for an address and a port. The argument handling will
    attempt to connect to the socket to ensure that it is a valid address/port and report any errors. This is then
    immediately closes the port after use. There is a minor race-condition between this check and the actual usage,
    however; it should be close enough.
    """

    DESCRIPTION = "Middleware options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Return arguments necessary to run a and connect to the GDS middleware"""
        zmq_arguments = {
            ("--no-zmq",): {
                "dest": "zmq",
                "action": "store_false",
                "help": "Disable ZMQ transportation layer, falling back to TCP socket server.",
                "default": True,
            },
            ("--zmq-transport",): {
                "dest": "zmq_transport",
                "nargs": 2,
                "help": "Pair of URLs used with --zmq to setup ZeroMQ transportation [default: %(default)s]",
                "default": [
                    f"ipc:///tmp/fprime-server-in-{getpass.getuser()}",
                    f"ipc:///tmp/fprime-server-out-{getpass.getuser()}",
                ],
                "metavar": ("serverInUrl", "serverOutUrl"),
            },
        }
        tts_arguments = {
            ("--tts-port",): {
                "dest": "tts_port",
                "action": "store",
                "type": int,
                "help": "Set the threaded TCP socket server port when ZMQ is not used [default: %(default)s]",
                "default": 50050,
            },
            ("--tts-addr",): {
                "dest": "tts_addr",
                "action": "store",
                "type": str,
                "help": "Set the threaded TCP socket server address when ZMQ is not used [default: %(default)s]",
                "default": "0.0.0.0",
            },
        }
        return {**zmq_arguments, **tts_arguments}

    def handle_arguments(self, args, **kwargs):
        """
        Checks to ensure that the specified port and address is available before connecting. This prevents user from
        attempting to run on a port that is unavailable.

        :param args: parsed argument namespace
        :return: args namespace
        """
        is_client = kwargs.get("client", False)
        tts_connection_address = (
            args.tts_addr.replace("0.0.0.0", "127.0.0.1")
            if is_client
            else args.tts_addr
        )

        args.connection_uri = f"tcp://{tts_connection_address}:{args.tts_port}"
        args.connection_transport = ThreadedTCPSocketClient
        if args.zmq:
            args.connection_uri = args.zmq_transport
            args.connection_transport = ZmqClient
        elif not is_client:
            check_port(args.tts_addr, args.tts_port)
        return args


class DictionaryParser(DetectionParser):
    """Parser for locating and loading dictionary information

    IMPORTANT: Since this parser loads global configuration that other parsers may depend on
    (only framing plugin at this time), it is recommended to list it first in any CompositeParser
    Not doing so would mean other parsers don't have access to dictionary config at handle_arguments time.

    This parser loads all dictionary elements and make them available for later use.
    It also updates the global ConfigManager with all type and constant definitions found
    in the dictionary.
    """

    DESCRIPTION = "Dictionary options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Arguments to handle dictionary."""
        return {
            **super().get_arguments(),
            **{
                ("--dictionary",): {
                    "dest": "dictionary",
                    "action": "store",
                    "default": None,
                    "required": False,
                    "type": str,
                    "help": "Path to dictionary. Overrides automatic dictionary detection.",
                },
                ("--packet-spec",): {
                    "dest": "packet_spec",
                    "action": "store",
                    "default": None,
                    "required": False,
                    "type": str,
                    "help": "Path to packet XML specification (should not be used if JSON packet definitions are used).",
                },
                ("--packet-set-name",): {
                    "dest": "packet_set_name",
                    "action": "store",
                    "default": None,
                    "required": False,
                    "type": str,
                    "help": "Name of packet set defined in the JSON dictionary.",
                },
            },
        }

    def handle_arguments(self, args, **kwargs):
        """Handle arguments as parsed"""
        # Find dictionary setting via "dictionary" argument or the "deploy" argument
        if args.dictionary is not None and not os.path.exists(args.dictionary):
            msg = f"Dictionary file {args.dictionary} does not exist"
            raise ValueError(msg)
        elif args.dictionary is None:
            args = super().handle_arguments(args, **kwargs)
            args.dictionary = find_dict(args.deployment)

        # Load dictionaries into global config and add it to args namespace
        args.dictionaries = Dictionaries.load_dictionaries_into_config(
            args.dictionary, args.packet_spec, args.packet_set_name
        )
        return args


class HashFileParser(ParserBase):
    """Fragment locating the hashes.txt file used for hash decoding, deriving it from the deployment when not given"""

    DESCRIPTION = "Hash file options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        return {
            ("--hash-file",): {
                "dest": "hash_file",
                "action": "store",
                "required": False,
                "type": str,
                "help": "Path to hashes.txt file map (found under build-artifacts dir by default)",
            }
        }

    def handle_arguments(self, args, **kwargs):
        if args.hash_file:
            args.hash_file = Path(args.hash_file)
            if not args.hash_file.exists():
                raise ValueError(f"hash file location {args.hash_file} does not exist")
        elif getattr(args, "deployment", None):
            hash_file = (Path(args.deployment) / ".." / ".." / "hashes.txt").resolve()
            args.hash_file = hash_file if hash_file.exists() else None
        return args


class FileHandlingParser(ParserBase):
    """Parser for deployments"""

    DESCRIPTION = "File handling options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Arguments to handle deployments"""

        username = getpass.getuser()

        return {
            ("--file-storage-directory",): {
                "dest": "files_storage_directory",
                "action": "store",
                "default": "/tmp/" + username,
                "required": False,
                "type": str,
                "help": "Directory to store uplink and downlink files. Default: %(default)s",
            },
            ("--file-uplink-cooldown",): {
                "dest": "file_uplink_cooldown",
                "action": "store",
                "default": 0,
                "required": False,
                "type": float,
                "help": "Cooldown period between file uplink packets. Default: %(default)s S",
            },
            ("--file-uplink-chunk-size",): {
                "dest": "file_uplink_chunk_size",
                "action": "store",
                "default": 256,
                "required": False,
                "type": int,
                "help": "Size of the data payload for a file uplink. Default: %(default)s",
            },
        }

    def handle_arguments(self, args, **kwargs):
        """Handle arguments as parsed"""
        try:
            Path(args.files_storage_directory).mkdir(parents=True, exist_ok=True)
        except PermissionError:
            raise PermissionError(
                f"{args.files_storage_directory} is not writable. Fix permissions or change storage directory with --file-storage-directory."
            )
        return args


class HistoryParser(ParserBase):
    """Parser for the pipeline's in-memory history behavior"""

    DESCRIPTION = "History options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Arguments controlling how the pipeline's history is retained"""
        return {
            ("--no-clear-history",): {
                "dest": "no_clear_history",
                "action": "store_true",
                "default": False,
                "help": "Do not clear history as it is retrieved by GDS clients (e.g. the web UI). By "
                "default, history is cleared once seen so a client connecting after data has "
                "already arrived (a race between the deployment starting and the UI loading) will "
                "miss it. Setting this retains all history for the lifetime of the process.",
            },
        }



class StandardPipelineParser(CompositeParser):
    """Standard pipeline argument parser: combination of MiddleWare and"""

    CONSTITUENTS = [
        DictionaryParser,
        HashFileParser,
        FileHandlingParser,
        MiddleWareParser,
        LogDeployParser,
        HistoryParser,
    ]

    def __init__(self):
        """Initialization"""
        super().__init__(
            constituents=self.CONSTITUENTS, description="Standard pipeline setup"
        )

    @staticmethod
    def pipeline_factory(args_ns, pipeline=None) -> StandardPipeline:
        """A factory of the standard pipeline given the handled arguments"""
        pipeline_arguments = {
            "config": ConfigManager.get_instance(),
            "dictionaries": args_ns.dictionaries,
            "file_store": args_ns.files_storage_directory,
            "logging_prefix": args_ns.logs,
            "data_logging_enabled": not args_ns.disable_data_logging,
            "cooldown": args_ns.file_uplink_cooldown,
            "chunk": args_ns.file_uplink_chunk_size,
        }
        pipeline = pipeline if pipeline else StandardPipeline()
        pipeline.transport_implementation = args_ns.connection_transport
        try:
            pipeline.setup(**pipeline_arguments)
            pipeline.connect(args_ns.connection_uri)
        except Exception:
            # In all error cases, pipeline should be shutdown before continuing with exception handling
            try:
                pipeline.disconnect()
            finally:
                raise
        return pipeline


class CommParser(CompositeParser):
    """Comm executable fragments; compose with `PluginArgumentParser` for the communication/framing plugin arguments"""

    CONSTITUENTS = [
        DictionaryParser,  # needed to get types from dictionary for framing
        CommExtraParser,
        MiddleWareParser,
        LogDeployParser,
    ]

    def __init__(self):
        """Initialization"""
        super().__init__(constituents=self.CONSTITUENTS, description="Communications bridge application")


class GdsParser(ParserBase):
    """
    Provides a parser for the following arguments:

    - dictionary: path to dictionary, either a folder for py_dicts, or a file for XML dicts
    - logs: path to logging path
    - config: configuration for GDS.

    Note: deployment can help in setting both dictionary and logs, but isn't strictly required.
    """

    DESCRIPTION = "GUI options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Return arguments necessary to run a binary deployment via the GDS"""
        return {
            ("-g", "--gui"): {
                "choices": GUIS,
                "dest": "gui",
                "type": str,
                "help": "Set the desired GUI system for running the deployment. [default: %(default)s]",
                "default": "html",
            },
            ("--gui-addr",): {
                "dest": "gui_addr",
                "action": "store",
                "default": "127.0.0.1",
                "required": False,
                "type": str,
                "help": "Set the GUI server address [default: %(default)s]",
            },
            ("--gui-port",): {
                "dest": "gui_port",
                "action": "store",
                "default": "5000",
                "required": False,
                "type": str,
                "help": "Set the GUI server address [default: %(default)s]",
            },
            ("--skip-browser-open",): {
                "dest": "browser_auto_open",
                "action": "store_false",
                "help": "Run server without auto-launching the default web browser",
            },
        }

    def handle_arguments(self, args, **kwargs):
        """
        Takes the arguments from the parser, and processes them into the needed map of key to dictionaries for the
        program. This will throw if there is an error.

        :param args: parsed args into a namespace
        :return: args namespace
        """
        return args


class BinaryDeployment(DetectionParser):
    """
    Parsing subclass used to read the arguments of the binary application. This derives functionality from a comm parser
    and represents the flight-side of the equation.
    """

    DESCRIPTION = "FPrime binary options"

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Return arguments necessary to run a binary deployment via the GDS"""
        return {
            **super().get_arguments(),
            **{
                ("-n", "--no-app"): {
                    "dest": "noapp",
                    "action": "store_true",
                    "default": False,
                    "help": "Do not run deployment binary. Overrides --app.",
                },
                ("--app",): {
                    "dest": "app",
                    "action": "store",
                    "required": False,
                    "type": str,
                    "help": "Path to app to run. Overrides automatic app detection.",
                },
                ("--application-arguments",): {
                    "dest": "application_arguments",
                    "nargs": "*",
                    "action": "extend",
                    "default": None,
                    "help": "Arguments to pass to the application binary, replacing the default -p/-a arguments. "
                    "Arguments starting with '-' must use the '--application-arguments=-x' form; a list in the "
                    "configuration file is passed through as-is.",
                },
            },
        }

    def handle_arguments(self, args, **kwargs):
        """
        Takes the arguments from the parser, and processes them into the needed map of key to dictionaries for the
        program. This will throw if there is an error.

        :param args: parsed arguments in namespace
        :return: args namespaces
        """
        # No app, stop processing now
        if args.noapp:
            return args
        args = super().handle_arguments(args, **kwargs)
        args.app = Path(args.app) if args.app else Path(find_app(args.deployment))
        if not args.app.is_file():
            msg = f"F prime binary '{args.app}' does not exist or is not a file"
            raise ValueError(msg)
        return args


class SearchArgumentsParser(ParserBase):
    """Parser for search arguments"""

    DESCRIPTION = "Searching and filtering options"

    def __init__(self, command_name: str) -> None:
        self.command_name = command_name

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Return arguments necessary to search through channels/events/commands"""
        return {
            ("--list",): {
                "dest": "is_printing_list",
                "action": "store_true",
                "help": f"list all possible {self.command_name[:-1]} types the current F Prime instance could produce, based on the {self.command_name} dictionary, sorted by {self.command_name[:-1]} type ID",
            },
            ("-i", "--ids"): {
                "dest": "ids",
                "action": "store",
                "required": False,
                "type": int,
                "nargs": "+",
                "help": f"only show {self.command_name} matching the given type ID(s) 'ID'; can provide multiple IDs to show all given types",
                "metavar": "ID",
            },
            ("-c", "--components"): {
                "dest": "components",
                "nargs": "+",
                "required": False,
                "type": str,
                "help": f"only show {self.command_name} from the given component name 'COMP'; can provide multiple components to show {self.command_name} from all components given",
                "metavar": "COMP",
            },
            ("-s", "--search"): {
                "dest": "search",
                "required": False,
                "type": str,
                "help": f'only show {self.command_name} whose name or output string exactly matches or contains the entire given string "STRING"',
            },
        }



class RetrievalArgumentsParser(ParserBase):
    """Parser for retrieval arguments"""

    DESCRIPTION = "Data retrieval options"

    def __init__(self, command_name: str) -> None:
        self.command_name = command_name

    def get_arguments(self) -> Dict[Tuple[str, ...], Dict[str, Any]]:
        """Return arguments to retrieve channels/events/commands in specific ways"""
        return {
            ("-t", "--timeout"): {
                "dest": "timeout",
                "action": "store",
                "required": False,
                "type": float,
                "help": f"wait at most SECONDS seconds for a single new {self.command_name[:-1]}, then exit (defaults to listening until the user exits via CTRL+C, and logging all {self.command_name})",
                "metavar": "SECONDS",
                "default": 0.0,
            },
            ("-j", "--json"): {
                "dest": "json",
                "action": "store_true",
                "required": False,
                "help": "returns response in JSON format",
            },
        }

