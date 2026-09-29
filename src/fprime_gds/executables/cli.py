"""
cli.py:

Command line handling for the F Prime GDS tools.

A tool's command line is assembled from `Fragment`s: plain declarations of argparse arguments, in the same
`{flags: kwargs}` form plugins return from `get_arguments`, plus an optional `handle(args, **kwargs)` function that runs
after parsing to validate or derive values. `parse_args` composes fragments into one argparse parser, applies the
`command-line-options` of the fprime-gds configuration file as the arguments' defaults, parses, and runs the handlers in
declaration order. `reproduce_arguments` turns a parsed namespace back into a command line for child processes.

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
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, List, Optional, Tuple

import yaml

# Required to set the checksum as a module variable
import fprime_gds.common.logger
from fprime_gds.common.communication.adapters.ip import check_port
from fprime_gds.common.models.dictionaries import Dictionaries
from fprime_gds.common.pipeline.standard import StandardPipeline
from fprime_gds.common.transport import ThreadedTCPSocketClient
from fprime_gds.common.utils.config_manager import ConfigManager
from fprime_gds.common.zmq_transport import ZmqClient
from fprime_gds.executables.utils import find_app, find_dict, get_artifacts_root
from fprime_gds.plugin.definitions import PluginType
from fprime_gds.plugin.system import Plugins, PluginsNotLoadedException

GUIS = ["none", "html"]

ArgumentSpecification = Dict[Tuple[str, ...], Dict[str, Any]]
Handler = Callable[..., None]

FLAG_ACTIONS = ("store_true", "store_false", "store_const")
LIST_ACTIONS = ("append", "extend")


class Fragment:
    """A reusable piece of a tool's command line

    `arguments` maps flag tuples (e.g. `("-d", "--deployment")`) to `argparse.add_argument` keyword arguments and is
    shown in help as one group titled `title`. `handle(args, **kwargs)`, when given, runs after parsing to validate or
    derive values in the namespace; raise ValueError to report a usage error. `kwargs` carry tool-wide flags such as
    `client=True` for tools connecting to a running GDS rather than hosting it. `parts` are fragments composed into this
    one; a fragment reached more than once when composing is used once.
    """

    def __init__(
        self,
        title: str,
        arguments: Optional[ArgumentSpecification] = None,
        handle: Optional[Handler] = None,
        parts: Iterable["Fragment"] = (),
    ):
        self.title = title
        self.arguments: ArgumentSpecification = dict(arguments or {})
        self.handle: Handler = handle if handle is not None else (lambda args, **kwargs: None)
        self.parts: List[Fragment] = list(parts)


def flatten(fragments: Iterable[Fragment]) -> List[Fragment]:
    """Fragments and their parts, depth first in declaration order, each fragment once"""
    ordered: List[Fragment] = []

    def visit(fragment: Fragment):
        if not any(fragment is seen for seen in ordered):
            ordered.append(fragment)
            for part in fragment.parts:
                visit(part)

    for fragment in fragments:
        visit(fragment)
    return ordered


def all_arguments(fragments: Iterable[Fragment]) -> ArgumentSpecification:
    """Arguments of fragments and their parts; the first declaration of a flag tuple wins"""
    merged: ArgumentSpecification = {}
    for fragment in flatten(fragments):
        for flags, keywords in fragment.arguments.items():
            merged.setdefault(flags, keywords)
    return merged


def long_flag(flags: Iterable[str]) -> str:
    """Best flag for an argument: the first --long flag, else the first flag"""
    flags = list(flags)
    return ([flag for flag in flags if flag.startswith("--")] + flags)[0]


def destination(flags: Iterable[str], keywords: Dict[str, Any]) -> str:
    """Namespace member an argument is stored into (mirrors argparse's dest derivation)"""
    return keywords.get("dest", re.sub(r"^-+", "", long_flag(flags)).replace("-", "_"))


def add_arguments(parser, arguments: ArgumentSpecification):
    """Add arguments to a parser or group, ignoring flags already added (e.g. two plugins declaring the same flag)"""
    for flags, keywords in arguments.items():
        try:
            parser.add_argument(*flags, **keywords)
        except argparse.ArgumentError:
            pass


def build_parser(fragments: Iterable[Fragment], description: str = "") -> argparse.ArgumentParser:
    """An argparse parser with one argument group per fragment, in declaration order"""
    parser = argparse.ArgumentParser(description=description, formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    for fragment in flatten(fragments):
        if fragment.arguments:
            add_arguments(parser.add_argument_group(title=fragment.title), fragment.arguments)
    return parser


def handle_arguments(fragments: Iterable[Fragment], args: argparse.Namespace, **kwargs) -> argparse.Namespace:
    """Run the handler of every fragment, in declaration order, returning the namespace"""
    for fragment in flatten(fragments):
        fragment.handle(args, **kwargs)
    return args


def parse_args(
    fragments: Iterable[Fragment],
    description: str = "No tool description provided",
    arguments: Optional[List[str]] = None,
    **kwargs,
) -> Tuple[argparse.Namespace, argparse.ArgumentParser]:
    """Parse and post-process a tool's command line

    Builds a parser from the configuration fragment and `fragments`, applies the configuration file's
    `command-line-options` as defaults, parses `arguments` (the command line when None), then runs the fragments'
    handlers in order. Configuration and handler errors print the usage and the error, then exit the process.

    Args:
        fragments: fragments making up the tool's command line
        description: tool description shown in help
        arguments: arguments to parse; None uses sys.argv[1:]
        **kwargs: passed to every handler (e.g. client=True)
    Returns: (namespace, argparse parser)
    """
    arguments = sys.argv[1:] if arguments is None else list(arguments)
    fragments = [CONFIGURATION, *fragments]
    parser = build_parser(fragments, description)
    try:
        configuration = load_configuration(arguments)
        apply_configuration(parser, configuration[1])
        args = handle_arguments(
            fragments, parser.parse_args(arguments), arguments=arguments, configuration=configuration, **kwargs
        )
    except Exception as exc:
        parser.print_usage(sys.stderr)
        print(f"[ERROR] Failed to parse arguments: {exc}", file=sys.stderr)
        sys.exit(-1)
    return args, parser


def extract_arguments(arguments: ArgumentSpecification, args: argparse.Namespace) -> Dict[str, Any]:
    """Values of the given arguments, as a {destination: value} dictionary (the plugin constructor keyword arguments)"""
    return {destination(flags, keywords): getattr(args, destination(flags, keywords)) for flags, keywords in arguments.items()}


def reproduce_arguments(fragments: Iterable[Fragment], args: argparse.Namespace) -> List[str]:
    """Command line reproducing the values of the fragments' optional arguments from a parsed namespace

    Used to hand a child process the values this process resolved. Values are passed as `--flag=value` so values
    beginning with '-' survive; flag arguments are emitted when set; positional arguments are not reproduced. Items an
    append/extend option received from the configuration file (`args.config_values`) are left to the child's read of
    that same file rather than appended a second time.
    """
    configured = (getattr(args, "config_values", None) or {}).get("command-line-options") or {}
    reproduced = []
    for flags, keywords in all_arguments(fragments).items():
        flag = long_flag(flags)
        action = keywords.get("action", "store")
        value = getattr(args, destination(flags, keywords), None)
        if not flag.startswith("-") or action in ("help", "version") or value is None:
            continue
        if action in LIST_ACTIONS and isinstance(value, list) and flag[2:] in configured:
            prefix = [str(item) for item in ([] if configured[flag[2:]] is None else configured[flag[2:]])]
            if not isinstance(configured[flag[2:]], list):
                prefix = [str(configured[flag[2:]])]
            if [str(item) for item in value[: len(prefix)]] == prefix:
                value = value[len(prefix):]
        if action in FLAG_ACTIONS:
            if value == keywords.get("const", action == "store_true"):
                reproduced.append(flag)
        elif action == "count":
            reproduced.extend([flag] * int(value))
        elif keywords.get("nargs") == "?" and value == keywords.get("const"):
            reproduced.append(flag)
        elif action in LIST_ACTIONS or keywords.get("nargs") is None or keywords.get("nargs") == "?":
            reproduced.extend(f"{flag}={item}" for item in (value if isinstance(value, (list, tuple)) else [value]))
        else:
            reproduced.extend([flag, *(str(item) for item in value)])
    return reproduced


####
# Configuration file
#
# The configuration file is YAML. Its `command-line-options` section (keyed by long flag name without dashes) supplies
# defaults for any tool's arguments; other sections (e.g. `flask`) are exposed to tools as `args.config_values`. The
# file is selected, in order of precedence, by -c/--config, the FPRIME_GDS_CONFIG_PATH environment variable, then
# `fprime-gds.yml` in the working directory. A file selected explicitly must exist; the implicit default may be absent.
####
DEFAULT_CONFIGURATION_PATH = Path("fprime-gds.yml")
CONFIGURATION_PATH_ENV = "FPRIME_GDS_CONFIG_PATH"
_default_configuration: Optional[Path] = DEFAULT_CONFIGURATION_PATH
_ignore_configuration_env = False


def set_default_configuration(path: Optional[Path]):
    """Replace the implicit configuration path (None disables it) and ignore the environment variable"""
    global _default_configuration, _ignore_configuration_env
    _default_configuration = path
    _ignore_configuration_env = True


def default_configuration() -> Optional[Path]:
    """Configuration path used when -c/--config is not supplied"""
    env_path = None if _ignore_configuration_env else os.environ.get(CONFIGURATION_PATH_ENV)
    return Path(env_path) if env_path else _default_configuration


def resolve_configuration(arguments: List[str]) -> Tuple[Optional[Path], bool]:
    """Configuration file for a command line, and whether it was requested explicitly (--config or environment)"""
    pre_parser = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
    pre_parser.add_argument("-c", "--config", type=Path, default=None)
    pre_parsed, _ = pre_parser.parse_known_args(arguments)
    if pre_parsed.config is not None:
        return pre_parsed.config, True
    env_path = None if _ignore_configuration_env else os.environ.get(CONFIGURATION_PATH_ENV)
    return (Path(env_path), True) if env_path else (_default_configuration, False)


def read_configuration(path: Optional[Path], explicit: bool) -> Dict[str, Any]:
    """Read a configuration file; an absent implicit file reads as {}, an absent explicit one is an error

    `!PATH` tagged values resolve relative to the configuration file's directory.
    """
    if path is None or (not Path(path).exists() and not explicit):
        return {}
    path = Path(path)
    if not path.exists():
        raise ValueError(f"Specified configuration file '{path}' does not exist")
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


def load_configuration(arguments: List[str]) -> Tuple[Optional[Path], Dict[str, Any]]:
    """Resolve and read the configuration for a command line: (path of the file read, or None; its contents)"""
    path, explicit = resolve_configuration(arguments)
    values = read_configuration(path, explicit)
    return (path if path is not None and Path(path).exists() else None), values


def configured_default(action: argparse.Action, value: Any) -> Any:
    """Convert a configuration value into a default for an argparse action

    Flag actions (nargs == 0) take their constant for None or True and the opposite for False. Optional-value actions
    (nargs == "?") take their constant for None, as a bare flag would. Other actions apply the action's `type` to the
    value, or to each item of a list for multi-valued actions.
    """
    if action.nargs == 0:
        if value is None or value is True:
            return action.const
        return (not action.const) if isinstance(action.const, bool) else action.default
    if action.nargs == argparse.OPTIONAL and value is None:
        return action.const
    convert = action.type if callable(action.type) else (lambda item: item)

    def convert_item(item):
        return item if item is None else convert(str(item))

    multi_valued = action.nargs not in (None, argparse.OPTIONAL) or isinstance(
        action, (argparse._AppendAction, argparse._ExtendAction)
    )
    if multi_valued:
        return [convert_item(item) for item in (value if isinstance(value, (list, tuple)) else [value])]
    converted = convert_item(value)
    if action.choices is not None and converted not in action.choices:
        choices = ", ".join(str(choice) for choice in action.choices)
        raise ValueError(f"configured value '{value}' for '{long_flag(action.option_strings)}' not in: {choices}")
    return converted


def apply_configuration(parser: argparse.ArgumentParser, config_values: Dict[str, Any]):
    """Make the configuration's `command-line-options` the defaults of the parser's arguments

    A configured option is no longer required. Options the parser does not know are ignored with a warning.
    """
    actions = {flag: action for action in parser._actions for flag in action.option_strings}
    for option, value in (config_values.get("command-line-options") or {}).items():
        action = actions.get(f"--{option}")
        if action is None or isinstance(action, (argparse._HelpAction, argparse._VersionAction)):
            print(f"[WARNING] Ignoring configured option '{option}' not supported by this tool", file=sys.stderr)
            continue
        action.default = configured_default(action, value)
        action.required = False


def _handle_configuration(args, arguments=(), configuration=None, **kwargs):
    """Expose the configuration as args.config (path of the file read, or None) and args.config_values

    `configuration` is the (path, values) pair already loaded by parse_args; it is loaded from `arguments` otherwise.
    """
    args.config, args.config_values = configuration if configuration is not None else load_configuration(list(arguments))


CONFIGURATION = Fragment(
    "Configuration options",
    {
        ("-c", "--config"): {
            "dest": "config",
            "type": Path,
            "default": None,
            "help": f"Configuration file path. [default: ${CONFIGURATION_PATH_ENV}, else {DEFAULT_CONFIGURATION_PATH}]",
        },
        ("-v", "--version"): {"action": "version", "version": version("fprime_gds")},
    },
    _handle_configuration,
)


####
# Deployment, dictionary, and binary
####
DEPLOYMENT_ARGUMENT = {
    ("-d", "--deployment"): {
        "dest": "deployment",
        "type": str,
        "help": "Deployment installation/build output directory. [default: install_dest field in settings.ini]",
    }
}


def detect_deployment(args) -> Path:
    """Locate the deployment (build output) directory from --deployment, else the build artifacts of the working directory"""
    if args.deployment:
        args.deployment = Path(args.deployment)
        return args.deployment
    detected_toolchain = get_artifacts_root() / platform.system()
    if not detected_toolchain.exists():
        raise Exception(f"{detected_toolchain} does not exist. Make sure to build.")
    likely_deployment = detected_toolchain / Path.cwd().name
    children = [child for child in detected_toolchain.iterdir() if child.is_dir()]
    if likely_deployment.exists():
        args.deployment = likely_deployment
    elif not children:
        raise Exception(f"No deployments found in {detected_toolchain}. Specify deployment with: --deployment")
    # Old structure where the bin, lib, and dict directories live immediately under the platform
    elif {child.name for child in children} == {"bin", "lib", "dict"}:
        args.deployment = detected_toolchain
    elif len(children) > 1:
        raise Exception(f"Multiple deployments found in {detected_toolchain}. Choose using: --deployment")
    else:
        args.deployment = children[0]
    return args.deployment


def _handle_dictionary(args, **kwargs):
    """Locate the dictionary (from --dictionary, else the deployment) and load it into the global configuration"""
    if args.dictionary is not None and not os.path.exists(args.dictionary):
        raise ValueError(f"Dictionary file {args.dictionary} does not exist")
    if args.dictionary is None:
        args.dictionary = find_dict(detect_deployment(args))
    args.dictionaries = Dictionaries.load_dictionaries_into_config(
        args.dictionary, args.packet_spec, args.packet_set_name
    )


DICTIONARY = Fragment(
    "Dictionary options",
    {
        **DEPLOYMENT_ARGUMENT,
        ("--dictionary",): {
            "dest": "dictionary",
            "type": str,
            "default": None,
            "help": "Path to dictionary. Overrides automatic dictionary detection.",
        },
        ("--packet-spec",): {
            "dest": "packet_spec",
            "type": str,
            "default": None,
            "help": "Path to packet XML specification (should not be used if JSON packet definitions are used).",
        },
        ("--packet-set-name",): {
            "dest": "packet_set_name",
            "type": str,
            "default": None,
            "help": "Name of packet set defined in the JSON dictionary.",
        },
    },
    _handle_dictionary,
)


def _handle_binary(args, **kwargs):
    """Locate the deployment binary unless --no-app"""
    if args.noapp:
        return
    args.app = Path(args.app) if args.app else Path(find_app(detect_deployment(args)))
    if not args.app.is_file():
        raise ValueError(f"F prime binary '{args.app}' does not exist or is not a file")


BINARY = Fragment(
    "FPrime binary options",
    {
        **DEPLOYMENT_ARGUMENT,
        ("-n", "--no-app"): {
            "dest": "noapp",
            "action": "store_true",
            "default": False,
            "help": "Do not run deployment binary. Overrides --app.",
        },
        ("--app",): {"dest": "app", "type": str, "help": "Path to app to run. Overrides automatic app detection."},
        ("--application-arguments",): {
            "dest": "application_arguments",
            "nargs": "*",
            "action": "extend",
            "default": None,
            "help": "Arguments to pass to the application binary, replacing the default -p/-a arguments. Arguments "
            "starting with '-' must use the '--application-arguments=-x' form; a list in the configuration file is "
            "passed through as-is.",
        },
    },
    _handle_binary,
)


def _handle_hash_file(args, **kwargs):
    """Locate hashes.txt from --hash-file, else next to the deployment's build artifacts"""
    if args.hash_file:
        args.hash_file = Path(args.hash_file)
        if not args.hash_file.exists():
            raise ValueError(f"hash file location {args.hash_file} does not exist")
    elif getattr(args, "deployment", None):
        hash_file = (Path(args.deployment) / ".." / ".." / "hashes.txt").resolve()
        args.hash_file = hash_file if hash_file.exists() else None


HASH_FILE = Fragment(
    "Hash file options",
    {
        ("--hash-file",): {
            "dest": "hash_file",
            "type": str,
            "help": "Path to hashes.txt file map (found under build-artifacts dir by default)",
        }
    },
    _handle_hash_file,
)


####
# Standard pipeline: middleware connection, logging, file handling, and history
####
def _handle_middleware(args, client=False, **kwargs):
    """Derive the transport (ZMQ or TCP server) and its URI; a hosting tool checks the TCP port is free"""
    tts_address = args.tts_addr.replace("0.0.0.0", "127.0.0.1") if client else args.tts_addr
    args.connection_uri = f"tcp://{tts_address}:{args.tts_port}"
    args.connection_transport = ThreadedTCPSocketClient
    if args.zmq:
        args.connection_uri = args.zmq_transport
        args.connection_transport = ZmqClient
    elif not client:
        check_port(args.tts_addr, args.tts_port)


MIDDLEWARE = Fragment(
    "Middleware options",
    {
        ("--no-zmq",): {
            "dest": "zmq",
            "action": "store_false",
            "default": True,
            "help": "Disable ZMQ transportation layer, falling back to TCP socket server.",
        },
        ("--zmq-transport",): {
            "dest": "zmq_transport",
            "nargs": 2,
            "default": [
                f"ipc:///tmp/fprime-server-in-{getpass.getuser()}",
                f"ipc:///tmp/fprime-server-out-{getpass.getuser()}",
            ],
            "metavar": ("serverInUrl", "serverOutUrl"),
            "help": "Pair of URLs used with --zmq to setup ZeroMQ transportation [default: %(default)s]",
        },
        ("--tts-port",): {
            "dest": "tts_port",
            "type": int,
            "default": 50050,
            "help": "Set the threaded TCP socket server port when ZMQ is not used [default: %(default)s]",
        },
        ("--tts-addr",): {
            "dest": "tts_addr",
            "type": str,
            "default": "0.0.0.0",
            "help": "Set the threaded TCP socket server address when ZMQ is not used [default: %(default)s]",
        },
    },
    _handle_middleware,
)


def _handle_logging(args, **kwargs):
    """Create the (dated, unless --log-directly) log directory and configure python logging into it"""
    if not args.log_directly:
        if args.log_prefix is None:
            tool_name = os.path.basename(sys.argv[0])
            args.log_prefix = tool_name[:-3] if tool_name.endswith(".py") else tool_name
        timestamp = datetime.datetime.now().strftime("%Y_%m_%d-%H_%M_%S")
        dir_name = f"{args.log_prefix}-{timestamp}" if args.log_prefix else timestamp
        args.logs = os.path.abspath(os.path.join(args.logs, dir_name))
        # A dated directory has been set, all log handling must now be direct
        args.log_directly = True
    try:
        os.makedirs(args.logs, exist_ok=True)
    except OSError as osexc:
        if osexc.errno != errno.EEXIST:
            raise
    fprime_gds.common.logger.configure_py_log(
        args.logs, mirror_to_stdout=args.log_to_stdout, log_level=args.log_level_gds
    )


LOGGING = Fragment(
    "Logging options",
    {
        ("-l", "--logs"): {
            "dest": "logs",
            "type": str,
            "default": os.path.join(os.getcwd(), "logs"),
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
            "type": str,
            "default": None,
            "help": "Prefix for log directory names (e.g. 'fprime-gds-<timestamp>'). Auto-detected from tool name when "
            "not specified. Use '' to disable. [default: auto-detect]",
        },
    },
    _handle_logging,
)


def _handle_file_handling(args, **kwargs):
    """Create the file storage directory"""
    try:
        Path(args.files_storage_directory).mkdir(parents=True, exist_ok=True)
    except PermissionError:
        raise PermissionError(
            f"{args.files_storage_directory} is not writable. Fix permissions or change storage directory with "
            "--file-storage-directory."
        )


FILE_HANDLING = Fragment(
    "File handling options",
    {
        ("--file-storage-directory",): {
            "dest": "files_storage_directory",
            "type": str,
            "default": "/tmp/" + getpass.getuser(),
            "help": "Directory to store uplink and downlink files. Default: %(default)s",
        },
        ("--file-uplink-cooldown",): {
            "dest": "file_uplink_cooldown",
            "type": float,
            "default": 0,
            "help": "Cooldown period between file uplink packets. Default: %(default)s S",
        },
        ("--file-uplink-chunk-size",): {
            "dest": "file_uplink_chunk_size",
            "type": int,
            "default": 256,
            "help": "Size of the data payload for a file uplink. Default: %(default)s",
        },
    },
    _handle_file_handling,
)

HISTORY = Fragment(
    "History options",
    {
        ("--no-clear-history",): {
            "dest": "no_clear_history",
            "action": "store_true",
            "default": False,
            "help": "Do not clear history as it is retrieved by GDS clients (e.g. the web UI). By default, history is "
            "cleared once seen so a client connecting after data has already arrived (a race between the deployment "
            "starting and the UI loading) will miss it. Setting this retains all history for the lifetime of the "
            "process.",
        },
    },
)

# DICTIONARY first: it loads the dictionary into the global configuration other handlers (e.g. framing plugins) read
STANDARD_PIPELINE = Fragment(
    "Standard pipeline setup", parts=[DICTIONARY, HASH_FILE, FILE_HANDLING, MIDDLEWARE, LOGGING, HISTORY]
)


def pipeline_factory(args: argparse.Namespace, pipeline: Optional[StandardPipeline] = None) -> StandardPipeline:
    """Set up and connect a StandardPipeline from a namespace handled by STANDARD_PIPELINE"""
    pipeline = pipeline if pipeline else StandardPipeline()
    pipeline.transport_implementation = args.connection_transport
    try:
        pipeline.setup(
            config=ConfigManager.get_instance(),
            dictionaries=args.dictionaries,
            file_store=args.files_storage_directory,
            logging_prefix=args.logs,
            data_logging_enabled=not args.disable_data_logging,
            cooldown=args.file_uplink_cooldown,
            chunk=args.file_uplink_chunk_size,
        )
        pipeline.connect(args.connection_uri)
    except Exception:
        # In all error cases, pipeline should be shutdown before continuing with exception handling
        try:
            pipeline.disconnect()
        finally:
            raise
    return pipeline


####
# Communications bridge and GUI
####
COMM_EXTRA = Fragment(
    "Communications options",
    {
        ("--output-unframed-data",): {
            "dest": "output_unframed_data",
            "nargs": "?",
            "default": None,
            "const": "unframed.log",
            "help": "Log unframed data to supplied file relative to log directory. Use '-' for standard out.",
        },
    },
)

# Compose with plugin_arguments() for the communication/framing plugin arguments
COMM = Fragment("Communications bridge application", parts=[DICTIONARY, COMM_EXTRA, MIDDLEWARE, LOGGING])

GUI = Fragment(
    "GUI options",
    {
        ("-g", "--gui"): {
            "dest": "gui",
            "choices": GUIS,
            "type": str,
            "default": "html",
            "help": "Set the desired GUI system for running the deployment. [default: %(default)s]",
        },
        ("--gui-addr",): {
            "dest": "gui_addr",
            "type": str,
            "default": "127.0.0.1",
            "help": "Set the GUI server address [default: %(default)s]",
        },
        ("--gui-port",): {
            "dest": "gui_port",
            "type": str,
            "default": "5000",
            "help": "Set the GUI server address [default: %(default)s]",
        },
        ("--skip-browser-open",): {
            "dest": "browser_auto_open",
            "action": "store_false",
            "help": "Run server without auto-launching the default web browser",
        },
    },
)


####
# Plugins
####
PLUGIN_DEFAULT_SELECTIONS = {
    "framing": "space-packet-space-data-link",
    "communication": "ip",
}


def disable_flag_destination(plugin) -> str:
    """Namespace member of a FEATURE plugin's --disable-<name> flag"""
    return f"disable_{plugin.get_name()}".lower().replace("-", "_")


def plugin_fragment(plugin, plugin_system: Plugins) -> Fragment:
    """One plugin's arguments (plus --disable-<name> for FEATURE plugins); handling checks them and binds the plugin

    Binding validates the plugin's own arguments via `check_arguments` and registers its implementor, with those
    arguments bound as constructor keywords, with the plugin system, ready for zero-argument construction.
    """
    arguments: ArgumentSpecification = {}
    if plugin.type == PluginType.FEATURE:
        arguments[(f"--disable-{plugin.get_name()}",)] = {
            "action": "store_true",
            "default": False,
            "dest": disable_flag_destination(plugin),
            "help": f"Disable the {plugin.category} plugin '{plugin.get_name()}'",
        }
    arguments.update(plugin.get_arguments())

    def handle(args, **kwargs):
        if plugin_system.get_category_plugin_type(plugin.category) == PluginType.SELECTION:
            try:
                plugin_system.get_selected_class(plugin.category)
                return  # Already bound (e.g. by an earlier parse)
            except PluginsNotLoadedException:
                pass
            if getattr(args, f"{plugin.category}_selection") != plugin.get_name():
                return
        elif getattr(args, disable_flag_destination(plugin), False):
            return
        values = extract_arguments(plugin.get_arguments(), args)
        plugin.check_arguments(**values)
        plugin_system.add_bound_class(plugin.category, functools.partial(plugin.get_implementor(), **values))

    return Fragment(f"{plugin.category.title()} Plugin '{plugin.get_name()}' Options", arguments, handle)


def plugin_arguments(plugin_system: Optional[Plugins] = None, defaults: Optional[Dict[str, str]] = None) -> Fragment:
    """Fragment of every plugin category's arguments

    SELECTION categories get a `--<category>-selection` flag (defaulting per `defaults`, else
    PLUGIN_DEFAULT_SELECTIONS, else the first plugin) and bind the selected plugin; FEATURE categories bind every plugin
    not disabled. Uses the plugin system singleton unless `plugin_system` is given.
    """
    plugin_system = plugin_system if plugin_system else Plugins.system()
    defaults = {**PLUGIN_DEFAULT_SELECTIONS, **(defaults or {})}
    categories = []
    for category in plugin_system.get_categories():
        plugins = list(plugin_system.get_plugins(category))
        arguments: ArgumentSpecification = {}
        if plugin_system.get_category_plugin_type(category) == PluginType.SELECTION:
            arguments[(f"--{category}-selection",)] = {
                "choices": [plugin.get_name() for plugin in plugins],
                "default": defaults.get(category, plugins[0].get_name()),
                "help": f"Select {category} implementer.",
            }
        categories.append(
            Fragment(
                f"{category.title()} Plugin Options",
                arguments,
                lambda args, category=category, **kwargs: plugin_system.start_loading(category),
                [plugin_fragment(plugin, plugin_system) for plugin in plugins],
            )
        )
    return Fragment("Plugin options", parts=categories)


####
# fprime-cli search and retrieval
####
def search_arguments(command_name: str) -> Fragment:
    """Arguments to search through channels/events/commands"""
    singular = command_name[:-1]
    return Fragment(
        "Searching and filtering options",
        {
            ("--list",): {
                "dest": "is_printing_list",
                "action": "store_true",
                "help": f"list all possible {singular} types the current F Prime instance could produce, based on the "
                f"{command_name} dictionary, sorted by {singular} type ID",
            },
            ("-i", "--ids"): {
                "dest": "ids",
                "type": int,
                "nargs": "+",
                "metavar": "ID",
                "help": f"only show {command_name} matching the given type ID(s) 'ID'; can provide multiple IDs to "
                "show all given types",
            },
            ("-c", "--components"): {
                "dest": "components",
                "nargs": "+",
                "type": str,
                "metavar": "COMP",
                "help": f"only show {command_name} from the given component name 'COMP'; can provide multiple "
                f"components to show {command_name} from all components given",
            },
            ("-s", "--search"): {
                "dest": "search",
                "type": str,
                "help": f'only show {command_name} whose name or output string exactly matches or contains the entire '
                'given string "STRING"',
            },
        },
    )


def retrieval_arguments(command_name: str) -> Fragment:
    """Arguments to retrieve channels/events/commands in specific ways"""
    return Fragment(
        "Data retrieval options",
        {
            ("-t", "--timeout"): {
                "dest": "timeout",
                "type": float,
                "default": 0.0,
                "metavar": "SECONDS",
                "help": f"wait at most SECONDS seconds for a single new {command_name[:-1]}, then exit (defaults to "
                f"listening until the user exits via CTRL+C, and logging all {command_name})",
            },
            ("-j", "--json"): {"dest": "json", "action": "store_true", "help": "returns response in JSON format"},
        },
    )
