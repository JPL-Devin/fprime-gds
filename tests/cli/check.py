#!/usr/bin/env python3
"""The command lines compared: `fprime-cli` of the Python GDS and the online subcommands of `fgds`.

Three parts (see README.md):
  definitions  `--list` of events, channels and commands, with and without filters
  live         `command-send` of the same commands against Ref; the packets on the link
  replay       the bytes Ref sent, fed to both ground systems while both command lines follow

Standard library only; the Python GDS is run through the executables of its virtual environment.
"""
import argparse
import difflib
import json
import os
import re
import signal
import socket
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
import scenario  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--fgds", required=True, help="an fgds binary that has the online subcommands")
parser.add_argument("--ref", required=True)
parser.add_argument("--dictionary", required=True)
parser.add_argument("--framing", choices=["ccsds", "fprime"], default="ccsds", help="the framing Ref was built with")
parser.add_argument("--venv-bin", required=True, help="bin directory of the fprime virtual environment")
parser.add_argument("--out", required=True)
args = parser.parse_args()
for name in ("fgds", "ref", "dictionary", "venv_bin", "out"):
    setattr(args, name, os.path.abspath(getattr(args, name)))
os.makedirs(args.out, exist_ok=True)
os.chdir(args.out)  # fprime-cli writes a `logs` directory where it runs
failures = []
PYTHON_CLI = os.path.join(args.venv_bin, "fprime-cli")
PYTHON_GDS = os.path.join(args.venv_bin, "fprime-gds")
ENV = {**os.environ, "PATH": args.venv_bin + os.pathsep + os.environ["PATH"]}


def check(what, python, rust):
    same = python == rust
    print(f"  {'same' if same else 'DIFFERENT'}: {what}")
    if not same:
        failures.append(what)
        pairs = zip(python, rust) if isinstance(python, list) and len(python) == len(rust) else [(python, rust)]
        first = next(pair for pair in pairs if pair[0] != pair[1])
        print(f"    fprime-cli: {json.dumps(first[0])[:700]}\n    fgds:       {json.dumps(first[1])[:700]}")


def require(what, condition, detail=""):
    print(f"  {'ok' if condition else 'FAILED'}: {what}")
    if not condition:
        failures.append(what)
        print(f"    {detail}"[:1500])


def run(command, **options):
    done = subprocess.run(command, capture_output=True, text=True, env=ENV, timeout=120, **options)
    return done.returncode, done.stdout, done.stderr


def json_lines(text):
    return [json.loads(line) for line in text.splitlines() if line.startswith("{")]


# The two ground systems, each with its command line.

class Fgds:
    follow_env = None

    def __init__(self, out):
        os.makedirs(out, exist_ok=True)
        http, self.link_port = scenario.free_port(), scenario.free_port()
        self.url = f"http://127.0.0.1:{http}"
        self.process = scenario.start(
            [args.fgds, "run", "--framing", args.framing, "--logs", f"{out}/logs", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}",
             "--comm-tcp-address", f"127.0.0.1:{self.link_port}"], open(f"{out}/gds.log", "w"))
        scenario.wait_for("fgds", lambda: self.get("/version"))

    def get(self, path):
        with urllib.request.urlopen(f"{self.url}/api/v1{path}", timeout=10) as response:
            return json.load(response)

    def cli(self, subcommand, *options):
        return [args.fgds, subcommand, "--url", self.url, *options]

    def follow(self, subcommand):
        return self.cli(subcommand, "--json")

    def send(self, name, arguments):
        return self.cli("command-send", name, "--arguments", *arguments)

    def received(self):
        return self.get("/status")["counters"].get("packets_received", 0)


class PythonGds:
    # fprime-cli --json stops on the first event with an enumeration argument ("not
    # JSON serializable"); tests/cli/patch makes its json.dumps write such values as text.
    follow_env = {**ENV, "PYTHONPATH": os.path.join(HERE, "patch")}

    def __init__(self, out):
        os.makedirs(out, exist_ok=True)
        self.out = out
        self.link_port, tts = scenario.free_port(), scenario.free_port()
        self.shared = ["--dictionary", args.dictionary, "--no-zmq", "--tts-port", str(tts), "--tts-addr", "127.0.0.1",
                       "--file-storage-directory", f"{out}/files"]
        self.process = scenario.start(
            [PYTHON_GDS, "-n", "-g", "none", "--ip-address", "127.0.0.1", "--ip-port", str(self.link_port),
             "--framing-selection", "fprime" if args.framing == "fprime" else "space-packet-space-data-link", "--logs", f"{out}/gds-logs", *self.shared],
            open(f"{out}/gds.log", "w"), env=ENV)
        scenario.wait_for("the Python GDS link port",
                          lambda: socket.create_connection(("127.0.0.1", self.link_port)).close() or True)
        time.sleep(2.0)  # its threaded TCP server starts after the link port

    def cli(self, subcommand, *options):
        return [PYTHON_CLI, subcommand, *self.shared, "--logs", f"{self.out}/cli-logs", *options]

    def follow(self, subcommand):
        return self.cli(subcommand, "-t", "0", "--json")

    def send(self, name, arguments):
        return self.cli("command-send", name, "--arguments", *arguments)


# 1. Definitions: neither command line needs a link for these; fprime-cli reads the
# dictionary file, fgds asks the running GDS.

def definitions(fgds):
    """The output of both command lines is the same text, byte for byte, in the text and the JSON form."""
    print("definitions: the --list of both command lines")
    dictionary = ["--dictionary", args.dictionary]
    filters = [[], ["-i", "16777217", "16777223"], ["-c", "CdhCore.cmdDisp"], ["-s", "ommand"], ["-c", "cdhcore.cmddisp"],
               ["-i", "999"], ["-s", "zzzzNOMATCH"], ["-c", "CdhCore.cmdDisp", "Ref.pingRcvr", "-s", "Ping"]]
    for subcommand in ("events", "channels", "command-send"):
        # README.md: the JSON form of `fprime-cli command-send --list` is a Python `repr`, not JSON.
        for form in ([], ["--json"]) if subcommand != "command-send" else ([],):
            for chosen in filters:
                code, python, _ = run([PYTHON_CLI, subcommand, "--list", *form, *dictionary, *chosen])
                also, rust, _ = run(fgds.cli(subcommand, "--list", *form, *chosen))
                assert python or chosen != filters[0], f"fprime-cli {subcommand} --list printed nothing"
                check(f"{subcommand} --list {' '.join(form + chosen)}: exit code and the {len(python.splitlines())} lines printed",
                      [code, *python.splitlines()], [also, *rust.splitlines()])
        for wrong in ("abc", "0x1000007"):
            code, _, python = run([PYTHON_CLI, subcommand, "--list", *dictionary, "-i", wrong])
            also, _, rust = run(fgds.cli(subcommand, "--list", "-i", wrong))
            last = lambda text: text.strip().splitlines()[-1].split(": ", 1)[1]
            check(f"{subcommand} --list -i {wrong}: exit code {code} and the error", [code, last(python)], [also, last(rust)])
    _, also, _ = run(fgds.cli("command-send", "--list"))
    _, rust, _ = run(fgds.cli("commands"))
    require("fgds command-send --list is fgds commands", also == rust and rust)
    for item in json_lines(run(fgds.cli("commands", "--json"))[1]):
        assert item["full_name"] and isinstance(item["args"], list), item
    names = [c["name"] for c in json.load(open(args.dictionary))["commands"]]
    for name in ("CdhCore.cmdDisp.CMD_NO_OPP", "Ref.nope.CMD", "cdhcore.cmddisp.cmd_no_op", "Ref.typeDemo.CHOICEZ", "x"):
        close = difflib.get_close_matches(name, names, n=3)
        expected = f"{name} is not a known command\n" + (f"Similar known commands: {close}\n" if close else "")
        code, output, _ = run(fgds.cli("command-send", name))
        check(f"unknown command {name}: exit code 0 and the text of fprime-cli, {len(close)} near names", [0, expected], [code, output])


# 2. Live: each command line sends the scenario's commands to Ref through its GDS.

def live(name, gds, out):
    ref = None
    try:
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(args.ref, proxy, out)
        code, sample, error = run(gds.cli("channels", "-t", "30", "--json"))
        assert json_lines(sample), f"{name}: no telemetry within 30 s: {code} {error}"
        for command, arguments in scenario.COMMANDS:
            code, output, error = run(gds.send(command, [json.dumps(a) if isinstance(a, (list, dict)) else str(a) for a in arguments]))
            assert code == 0, f"{name}: command-send {command} failed: {output}{error}"
            time.sleep(scenario.GAP)
        time.sleep(scenario.SETTLE)
        return bytes(proxy.uplink), bytes(proxy.downlink)
    finally:
        if ref:
            scenario.stop(ref)


# 2b. The dash matrix: command lines whose tokens start with `-`, through both command lines against Ref.
# Each is read by argparse of `fprime-cli command-send`; fgds reads it by the same rule (`fgds_core::argparse`,
# checked row by row against `fixtures/command-line/table.json`). Here the end: exit code, standard output,
# the message after `error: ` and the command packets on the link.

F32, I8, TEXT, THREE = ("Ref.sendBuffComp.PARAMETER4_PRM_SET", "Ref.sendBuffComp.PARAMETER3_PRM_SET",
                        "CdhCore.cmdDisp.CMD_NO_OP_STRING", "CdhCore.cmdDisp.CMD_TEST_CMD_1")
DASHES = [
    # Round 7, P1: a negative number is `^-\d+$|^-\d*\.\d+$`, with the digits of any script.
    [F32, "--arguments", "-.5"], [F32, "--arguments", "-\u0665"], [F32, "--arguments", "-\u0665.\u0665"],
    [TEXT, "--arguments", "-\u0665"], [F32, "--arguments", "-5."], [I8, "--arguments", "-5"], [THREE, "--arguments", "-1", "-2.5", "3"],
    # P2: `-` alone, and a token with a space.
    [TEXT, "--arguments", "-"], [TEXT, "--arguments", "- 5"], [TEXT, "--arguments", "-x y"], [TEXT, "--arguments", ""],
    # P3: the value written with `=`.
    [F32, "--arguments=-inf"], [F32, "--arguments=-Infinity"], [F32, "--arguments=-NaN"], [F32, "--arguments=-1e-400"],
    [F32, "--arguments=-x"], [TEXT, "--arguments=a=b"], [TEXT, "--arguments="], [F32, "--arg=-1.5"],
    # P4: `--`.
    [I8, "--arguments", "--"], [F32, "--arguments", "--", "-inf"], [F32, "--arguments", "1.5", "--"], [F32, "--", "--arguments", "1.5"],
    [TEXT, "--arguments", "---"],
    # P5: an option of the parser where an argument is meant.
    [I8, "--arguments", "-l"], [I8, "--arguments", "-s"], [I8, "--arguments", "-i"], [I8, "--arguments", "-c"], [I8, "--arguments", "-d"],
    [I8, "--arguments", "-j"], [I8, "--arguments", "-h"],
    # Round 7 web, N4: `-inf` with a value after it, and the name after `--`.
    [THREE, "--arguments", "5", "-inf", "7"], [THREE, "--arguments", "-inf", "-inf"], [THREE, "--arguments", "1", "-inf", "-inf"], [THREE, "--arguments", "5", "1.5", "-inf"], ["--arguments", "5", "-inf", "7", "--", THREE],
    [THREE, "--arguments", "5", "-i", "7"],
    # P6: `-j` with a command that is sent.
    [F32, "-j", "--arguments", "1.5"], [F32, "--arguments", "1.5", "--json"],
    # Round 6 and the M2 row: what the parser refuses or reads as an option.
    *[[F32, "--arguments", text] for text in (
        "-inf", "-infinity", "-Infinity", "-NaN", "-nan", "-1e-400", "-1e308", "-0x10", "-1_0", "-1e+5", "-x", "--bogus", "-1e",
        "-1-2", "-1,5", "-a", "-t", "-I", "-N", "-+1", "--inf", "-5\n")],
    [THREE, "--arguments", "-2147483648", "-3.4e38", "255"],
    # Long options shortened.
    [F32, "--arg", "1.5"], [F32, "--a", "2.5"], [F32, "--arguments", "1.5", "--js"], [F32, "--arguments", "1.5", "--j"],
    [F32, "--arguments", "1.5", "--d"], [F32, "--arguments", "1.5", "--no"], [F32, "--arguments", "1.5", "--l"],
    [F32, "--arguments", "1.5", "--se", "x"], [F32, "--arguments", "1.5", "--comp", "x", "y"], [F32, "--arguments", "1.5", "--id", "5"],
    [F32, "--arguments", "1.5", "--id=x"], [F32, "--arguments", "1.5", "--log-level-gds", "x"], [F32, "--arguments", "1.5", "--log-level-gds=INFO"],
    [F32, "--arguments", "1.5", "--file-uplink-cooldown", "x"], [F32, "--arguments", "1.5", "--zmq-transport", "a"],
    # A value attached to a short option, short options together.
    [F32, "-i5", "--arguments", "1.5"], [F32, "--arguments", "1.5", "-i5"], [F32, "--arguments", "1.5", "-i5", "6"],
    [F32, "--arguments", "1.5", "-i=5"], [F32, "--arguments", "1.5", "-i5.5"], [F32, "--arguments", "1.5", "-i-5"],
    [F32, "--arguments", "1.5", "-i", "-5"], [F32, "--arguments", "1.5", "-sx"], [F32, "--arguments", "1.5", "-s=x"],
    [F32, "--arguments", "1.5", "-cx", "y"], [F32, "--arguments", "1.5", "-jsx"], [F32, "--arguments", "1.5", "-ji5"],
    [F32, "--arguments", "1.5", "-jj"], [F32, "--arguments", "1.5", "-j5"], [F32, "--arguments", "1.5", "-j=5"], [F32, "--arguments", "1.5", "-jx"],
    # Options given again.
    [F32, "--arguments", "1.5", "--arguments", "2.5"], [THREE, "--arguments", "1", "2", "--arguments", "3", "-4.5", "5"],
    [F32, "--arguments", "1.5", "--arguments"], [F32, "--arguments", "1.5", "-j", "-j"], [F32, "-i", "1", "-i", "2", "--arguments", "3.5"],
    [F32, "-s", "a", "--arguments", "3.5", "-s", "b"],
    # Options after and between positionals.
    ["--arguments", "1.5", F32], ["--arguments", "1.5", "-j", F32], ["-j", F32, "--arguments", "1.5"], ["--arguments", "1.5", "--", F32],
    [F32, "x", "--arguments", "1.5"], [F32, "--arguments", "1.5", "-j", "2.5"], [F32, "-5", "--arguments", "1.5"], ["-5", "--arguments", "1.5"],
    ["-j"], [],
]

# Round 8, Q1: every beginning of a long option of `fprime-cli command-send` that several options have.
# `ambiguous option` lists them in the order of the parser. fprime-cli keeps its six groups of GDS options in
# a set, so the order of the groups changes from start to start: the beginnings in VARYING match options of
# two groups or more, and their names are compared as a set. The others are compared as written.
NO_OP = "CdhCore.cmdDisp.CMD_NO_OP"
LONG = ["--help", "--hash-file", "--file-storage-directory", "--file-uplink-cooldown", "--file-uplink-chunk-size",
        "--no-zmq", "--zmq-transport", "--tts-port", "--tts-addr", "--logs", "--log-directly", "--log-to-stdout",
        "--log-level-gds", "--disable-data-logging", "--log-prefix", "--no-clear-history", "--deployment", "--dictionary",
        "--packet-spec", "--packet-set-name", "--arguments", "--list", "--ids", "--components", "--search", "--json"]
AMBIGUOUS = sorted({name[:end] for name in LONG for end in range(3, len(name))
                    if sum(other.startswith(name[:end]) for other in LONG) > 1})
VARYING = ["--d", "--di", "--n", "--no", "--no-", "--="]
DASHES += [[NO_OP, beginning] for beginning in AMBIGUOUS + ["--="]]

# Round 8, Q2: an option of fprime-cli that names something that is not there. Both end with the same exit
# code and send nothing; the texts differ (deviations.md).
MISSING = [
    ["--dictionary", "/nonexistent.json"], ["--dictionary", "/tmp"], ["--dictionary", "{out}/not-json"], ["--dictionary", "{out}/empty.json"],
    ["--packet-set-name", "x"], ["--packet-spec", "/nonexistent"],
    ["--hash-file", "/nonexistent"], ["--file-storage-directory", "/nonexistent/x"], ["--file-storage-directory", "{out}/not-json"],
    ["-l", "/nonexistent/logs"], ["-l", "{out}/not-json"], ["--log-prefix", "/nonexistent/p"],
]
# The same options naming something that is there, and the options without a use in fgds: the command is sent.
THERE = [
    # A deployment is where the dictionary is looked for: not looked at when `--dictionary` is given.
    ["--dictionary", "{dictionary}", "-d", "/nonexistent"],
    ["--packet-set-name", "RefPackets"], ["--hash-file", "{out}/not-json"], ["--file-storage-directory", "{out}/files"],
    ["--log-prefix", "p"], ["--file-uplink-cooldown", "-1"], ["--file-uplink-chunk-size", "0"], ["--log-directly"],
    ["--log-level-gds", "DEBUG"], ["--disable-data-logging"], ["--no-clear-history"], ["--log-to-stdout"],
]


def named(out):
    """MISSING and THERE as command lines, their files made."""
    os.makedirs(out, exist_ok=True)
    open(f"{out}/not-json", "w").write("x\n")
    open(f"{out}/empty.json", "w").write("{}\n")
    return [[NO_OP, *(token.format(out=out, dictionary=args.dictionary) for token in options)] for options in MISSING + THERE]


def dashes(name, gds, out, lines=None):
    """Each command line of DASHES: exit code, standard output, the message, the command packets it put on the link."""
    lines = DASHES if lines is None else lines
    ref, results, scid = None, [], scenario.spacecraft_id(args.dictionary)
    try:
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(args.ref, proxy, out)
        code, sample, error = run(gds.cli("channels", "-t", "30", "--json"))
        assert json_lines(sample), f"{name}: no telemetry within 30 s: {code} {error}"
        time.sleep(scenario.GAP)
        for tokens in lines:
            before = len(scenario.uplink_packets(bytes(proxy.uplink), args.framing, scid))
            code, output, error = run(gds.cli("command-send", *tokens))
            time.sleep(scenario.GAP)
            packets = [p.hex() for p in scenario.uplink_packets(bytes(proxy.uplink), args.framing, scid)[before:]]
            message = error.split(": error: ", 1)[1].strip() if code == 2 and ": error: " in error else ""
            if message.startswith("ambiguous option: ") and tokens[-1] in VARYING:
                message = message.split(" could match ")[0] + " could match " + ", ".join(sorted(message.split(" could match ")[1].split(", ")))
            results.append({"exit": code, "stdout": output, "message": message, "packets": packets})
        return results
    finally:
        if ref:
            scenario.stop(ref)


# The two differences of docs/design/deviations.md that the matrix meets.
HELP = [[I8, "--arguments", "-h"]]  # both print their own help text
# C4: fewer arguments left than the command takes
COUNT = [[I8, "--arguments", "-j"], [F32, "--arguments", "1.5", "--arguments"], [THREE, "--arguments", "5", "-i", "7"]]


def compare_dashes(python, rust):
    for tokens, expected, got in zip(DASHES, python, rust):
        what = f"command-send {json.dumps(tokens, ensure_ascii=True)}"
        if tokens in HELP:
            require(f"{what}: both print a help text, exit code 0, nothing sent",
                    all(r["exit"] == 0 and r["stdout"] and not r["packets"] for r in (expected, got)), f"{expected} {got}")
        elif tokens in COUNT:
            require(f"{what}: fprime-cli sends the command with too few arguments, fgds refuses it with exit code 1 (C4)",
                    expected["exit"] == 0 and len(expected["packets"]) == 1 and got == {"exit": 1, "stdout": "", "message": "", "packets": []},
                    f"{expected} {got}")
        else:
            check(f"{what}: exit {expected['exit']}, {len(expected['packets'])} packet(s)", expected, got)
    sent = sum(1 for result in python if result["packets"])
    require(f"the dash matrix has command lines that send and command lines that do not: {sent} of {len(python)} sent",
            20 <= sent <= len(python) - 20)


def compare_named(lines, python, rust):
    for index, (tokens, expected, got) in enumerate(zip(lines, python, rust)):
        what = f"command-send {' '.join(tokens[1:])}"
        if index < len(MISSING):
            require(f"{what}: both end with exit code {expected['exit']} and send nothing",
                    expected["exit"] not in (0, 2) and got["exit"] == expected["exit"] and not expected["packets"] and not got["packets"]
                    and not got["stdout"], f"{expected} {got}")
        elif tokens[-1] == "--log-to-stdout":
            require(f"{what}: both send the command, exit code 0; fprime-cli prints the lines of its logging, fgds nothing",
                    expected["exit"] == got["exit"] == 0 and expected["packets"] == got["packets"] and len(got["packets"]) == 1
                    and "Logging system initialized!" in expected["stdout"] and not got["stdout"], f"{expected} {got}")
        else:
            check(f"{what}: exit 0, 1 packet", expected, got)
            require(f"{what}: the command is sent", got["exit"] == 0 and len(got["packets"]) == 1, f"{got}")


def refused(fgds):
    """What fgds answers to commands that cannot be sent, and its dry run."""
    code, _, error = run(fgds.cli("command-send"))
    require("no command name: exit code 2 and the error of fprime-cli",
            code == 2 and error.strip() == "fgds: error: One of command-name or --list is required", error)
    code, _, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", "-2147483648", "-3.4e38", "255"))
    require("an argument `-3.4e38` ends the command line as with argparse: exit code 2, nothing sent",
            code == 2 and error.strip() == "fgds: error: unrecognized arguments: -3.4e38 255", error)
    code, _, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", "1"))
    require("a command with too few arguments is refused and its arguments are listed",
            code != 0 and "3 arguments" in error and "arg2 (F32Type)" in error, error)
    code, _, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", "1", "x", "3"))
    require("a command with an argument of the wrong type is refused", code != 0 and "x" in error, error)
    code, _, error = run(fgds.cli("events", "-t", "abc"))
    require("a timeout that is no number: exit code 2 and the error of fprime-cli",
            code == 2 and error.strip() == "fgds: error: argument -t/--timeout: invalid float value: 'abc'", error)
    code, output, error = run(fgds.cli("events", "-t", "1", "-s", "no event holds this text"))
    require("a timeout with nothing printed ends with exit code 0, as fprime-cli", code == 0 and output == "", f"{code} {error}")
    endless = subprocess.Popen(fgds.cli("events", "-t", "-1", "-s", "no event holds this text"), env=ENV,
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    try:
        endless.wait(timeout=3)
        require("a negative timeout follows without end, as fprime-cli", False, endless.stderr.read())
    except subprocess.TimeoutExpired:
        require("a negative timeout follows without end, as fprime-cli", True)
    finally:
        endless.kill()
        endless.wait()
    for argument, message in (
        ("-inf", "argument -i/--ids: invalid int value: 'nf'"),
        ("-infinity", "argument -i/--ids: invalid int value: 'nfinity'"),
    ):
        for name in ("Ref.sendBuffComp.PARAMETER3_PRM_SET", "Ref.sendBuffComp.PARAMETER4_PRM_SET"):
            code, _, error = run(fgds.cli("command-send", name, "--arguments", argument))
            require(f"{name} {argument}: read as `-i`, exit code 2 and the error of fprime-cli",
                    code == 2 and error.strip() == f"fgds: error: {message}", f"{code} {error}")
    # fprime-cli: exit code 2 and `fprime-cli: error: unrecognized arguments: <argument>`.
    for argument in ("-Infinity", "-NaN", "-1e-400", "-1e308"):
        code, _, error = run(fgds.cli("command-send", "Ref.sendBuffComp.PARAMETER3_PRM_SET", "--arguments", argument))
        require(f"PARAMETER3_PRM_SET {argument}: exit code 2", code == 2, f"{code} {error}")
    # A background job of a shell script without job control has SIGINT ignored: fprime-cli keeps running.
    script = 'trap "" INT; "$@" & pid=$!; sleep 2; kill -INT $pid; sleep 2; kill -0 $pid; alive=$?; kill -KILL $pid; exit $alive'
    for subcommand in ("events", "channels"):
        code, _, error = run(["sh", "-c", script, "sh", *fgds.cli(subcommand)])
        require(f"{subcommand} started with SIGINT ignored keeps running after SIGINT, as fprime-cli", code == 0, f"{code} {error}")
    # "Normally": with the default action for SIGINT, whatever this check itself was started with.
    interrupted = subprocess.Popen(fgds.cli("events"), env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
    time.sleep(2)
    interrupted.send_signal(signal.SIGINT)
    require("events started normally ends with exit code 0 on SIGINT", interrupted.wait(timeout=10) == 0)
    scalars = dict(i8=1, i16=2, i32=3, i64=4, u8="7", u16=6, u32=7, u64=8, f32=1.5, f64=2.5)
    code, _, error = run(fgds.cli("command-send", "Ref.typeDemo.SEND_SCALARS", "--arguments", json.dumps(scalars)))
    require("a text where the JSON argument holds a number is refused with the message of fprime-cli, nothing sent",
            code == 1 and "Type <class 'int'> expected, type <class 'str'> found!" in error.splitlines()[0]
            and fgds.get("/records?kind=fprime.command&limit=10000")["records"][-1]["body"]["outcome"] == "rejected", error)
    # An integer literal beyond 64 bits is an integer in Python: out of range, not a float.
    for member, literal in (("u64", "18446744073709551616"), ("i64", "-9223372036854775809"), ("u8", "1" + "0" * 40)):
        text = json.dumps({**scalars, "u8": 7, member: 0}).replace(f'"{member}": 0', f'"{member}": {literal}')
        code, _, error = run(fgds.cli("command-send", "Ref.typeDemo.SEND_SCALARS", "--arguments", text))
        require(f"{member} {literal} in a JSON argument is refused as out of range, nothing sent",
                code == 1 and "out of range" in error.splitlines()[0] and "expected, type" not in error
                and fgds.get("/records?kind=fprime.command&limit=10000")["records"][-1]["body"]["outcome"] == "rejected", error)
    code, _, error = run(fgds.cli("command-send", "Ref.typeDemo.SEND_SCALARS", "--arguments",
                                  json.dumps({**scalars, "u8": 7, "f64": 0}).replace('"f64": 0', '"f64": 18446744073709551616')))
    sent = fgds.get("/records?kind=fprime.command&limit=10000")["records"][-1]["body"]
    require("an integer beyond 64 bits is taken for a float member", "out of range" not in error and sent["args"]["scalar_input"]["f64"] == 2.0 ** 64, f"{error} {sent}")
    # Command line round 5, N2 to N5. The table of every boundary against the Python GDS is in the tests of
    # `fgds-dictionary` (`fixtures/command-args/json-table.json`); here the command line itself.
    def scalars_with(member, literal):
        text = json.dumps({**scalars, "u8": 7, member: 0}).replace(f'"{member}": 0', f'"{member}": {literal}')
        code, _, error = run(fgds.cli("command-send", "Ref.typeDemo.SEND_SCALARS", "--arguments", text))
        return code, error, fgds.get("/records?kind=fprime.command&limit=10000")["records"][-1]["body"]
    code, error, sent = scalars_with("u64", '{"\\u0000int": "5"}')
    require("an object is not an integer, whatever its member is called",
            code == 1 and "Type <class 'int'> expected, type <class 'dict'> found!" in error.splitlines()[0]
            and sent["outcome"] == "rejected", f"{error} {sent}")
    for member, literal in (("f64", "1" + "0" * 400), ("f64", "-1" + "0" * 400), ("f64", str(2 ** 1024)), ("f32", "1" + "0" * 400)):
        code, error, sent = scalars_with(member, literal)
        require(f"an integer of {len(literal)} characters for {member} is refused, nothing sent",
                code == 1 and sent["outcome"] == "rejected", f"{error} {sent}")
    for literal, value in (("1e400", "Infinity"), ("-1e400", "-Infinity"), ("Infinity", "Infinity"), ("NaN", "NaN")):
        code, error, sent = scalars_with("f64", literal)
        # No Ref in this part: the argument is taken and only the link refuses.
        require(f"the float literal {literal} is taken as {value}",
                sent["args"]["scalar_input"]["f64"] == value and (code == 0 or sent.get("reason") == "link down"),
                f"{error} {sent}")
    for literal in (str(2 ** 128), str(-2 ** 127 - 1), "1" + "0" * 60):
        code, error, sent = scalars_with("u64", literal)
        require(f"the refusal of {literal} names the number as given",
                code == 1 and f"{literal} is out of range for U64" in error.splitlines()[0], error)
    before = len(fgds.get("/records?kind=fprime.command&limit=10000")["records"])
    code, packet, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--dry-run", "--arguments", "1", "-2.5", "3"))
    after = len(fgds.get("/records?kind=fprime.command&limit=10000")["records"])
    require("a dry run prints the packet and sends nothing",
            code == 0 and packet.strip() == "00000100000200000001c020000003" and before == after, f"{code} {packet} {error}")


# 3. Replay: the same bytes into both ground systems, while both command lines follow.

def follow_replay(name, gds, capture, out):
    frames = scenario.frames(capture, args.framing, scenario.tm_frame_size(args.dictionary))
    followers = {
        subcommand: subprocess.Popen(gds.follow(subcommand), stdout=open(f"{out}/{subcommand}.jsonl", "w"),
                                     stderr=open(f"{out}/{subcommand}.err", "w"), env=gds.follow_env)
        for subcommand in ("events", "channels")
    }
    try:
        time.sleep(4.0)  # both followers are connected before the first byte
        flight = scenario.wait_for("the link", lambda: socket.create_connection(("127.0.0.1", gds.link_port)))
        # One frame at a time: fprime-cli polls, and takes one item per round.
        for frame in frames:
            flight.sendall(frame)
            time.sleep(0.02)
        if hasattr(gds, "received"):
            scenario.steady(gds.received)
        time.sleep(5.0)
        for subcommand, follower in followers.items():
            assert follower.poll() is None, f"{name} {subcommand} stopped: {open(f'{out}/{subcommand}.err').read()}"
        flight.close()
    finally:
        for follower in followers.values():
            follower.send_signal(signal.SIGINT)
        for follower in followers.values():
            try:
                follower.wait(timeout=10)
            except subprocess.TimeoutExpired:
                follower.kill()
    return {subcommand: json_lines(open(f"{out}/{subcommand}.jsonl").read()) for subcommand in followers}


def received_event(item):
    return {"id": item["id"], "name": item["name"], "severity": item["severity"].replace("EventSeverity.", ""),
            "time": item["raw_time"], "args": scenario.plain(list(item["args"].values()) if isinstance(item["args"], dict) else item["args"]),
            "display_text": item["display_text"]}


def received_channel(item):
    return {"id": item["id"], "name": item["name"], "time": item["raw_time"], "display_text": str(item["display_text"])}


def subset(what, python, rust):
    """Every item of fprime-cli is an item of fgds (README.md: fprime-cli skips items and orders by time)."""
    # README.md, accepted difference 1: an item fprime-cli printed twice counts once.
    missing = [item for item in python if item not in rust]
    require(f"{what}: each of the {len(python)} items fprime-cli printed is among the {len(rust)} of fgds, same values",
            python and not missing, f"not printed by fgds: {json.dumps(missing[:2])}\n    fgds has e.g.: {json.dumps(rust[:2])}")


def main():
    os.makedirs(args.out, exist_ok=True)
    fgds = python = None
    try:
        fgds = Fgds(f"{args.out}/rust")
        definitions(fgds)

        print("live: command-send of both command lines against Ref")
        rust_uplink, capture = live("fgds", fgds, f"{args.out}/rust")
        refused(fgds)
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python")
        python_uplink, _ = live("fprime-cli", python, f"{args.out}/python")
        scenario.stop(python.process)
        print(f"dash matrix: {len(DASHES)} command lines of command-send through both against Ref")
        fgds = Fgds(f"{args.out}/rust-dashes")
        rust_dashes = dashes("fgds", fgds, f"{args.out}/rust-dashes")
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python-dashes")
        compare_dashes(dashes("fprime-cli", python, f"{args.out}/python-dashes"), rust_dashes)
        scenario.stop(python.process)
        lines = named(f"{args.out}/named")
        print(f"options that name a file, a directory or a packet set: {len(lines)} command lines through both against Ref")
        fgds = Fgds(f"{args.out}/rust-named")
        rust_named = dashes("fgds", fgds, f"{args.out}/rust-named", lines)
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python-named")
        compare_named(lines, dashes("fprime-cli", python, f"{args.out}/python-named", lines), rust_named)
        scenario.stop(python.process)
        # Without `--dictionary`, so without the options every other fprime-cli line of this check has: no GDS is asked.
        for options in (["-d", "/nonexistent"], ["--deployment", f"{args.out}/named"]):
            expected = run([PYTHON_CLI, "command-send", NO_OP, *options])
            got = run([args.fgds, "command-send", NO_OP, "--url", "http://127.0.0.1:1", *options])
            require(f"command-send {' '.join(options)}, no --dictionary: both end with exit code 255",
                    expected[0] == got[0] == 255 and not got[1], f"{expected} {got}")
        open(f"{args.out}/downlink.bin", "wb").write(capture)
        scid = scenario.spacecraft_id(args.dictionary)
        packets = [[p.hex() for p in scenario.uplink_packets(stream, args.framing, scid)] for stream in (python_uplink, rust_uplink)]
        assert len(packets[0]) == len(scenario.COMMANDS), f"fprime-cli put {len(packets[0])} packets on the link"
        check(f"the {len(packets[0])} command packets on the link, byte for byte", *packets)

        frames = len(scenario.frames(capture, args.framing, scenario.tm_frame_size(args.dictionary)))
        print(f"replay: {frames} frames of Ref into both, both command lines following")
        fgds = Fgds(f"{args.out}/rust-replay")
        rust = follow_replay("fgds", fgds, capture, f"{args.out}/rust-replay")
        history = {s: json_lines(run(fgds.cli(s, "--history", "--no-follow", "--json"))[1]) for s in ("events", "channels")}
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python-replay")
        followed = follow_replay("fprime-cli", python, capture, f"{args.out}/python-replay")
        scenario.stop(python.process)
        for subcommand, shape in (("events", received_event), ("channels", received_channel)):
            require(f"fgds {subcommand}: following printed the {len(history[subcommand])} items that --history lists afterwards",
                    sorted(map(json.dumps, rust[subcommand])) == sorted(map(json.dumps, history[subcommand])) and rust[subcommand])
            # The order of fprime-cli: by flight time (README.md, 2).
            times = [tuple(map(int, re.fullmatch(r"\(\d+\(\d+\)-(\d+):(\d+)\)", i["raw_time"]).groups())) for i in history[subcommand]]
            require(f"fgds {subcommand} --history: the {len(times)} items are in the order of their flight times", times == sorted(times))
            subset(subcommand, [shape(i) for i in followed[subcommand]], [shape(i) for i in rust[subcommand]])
    finally:
        for gds in (fgds, python):
            if gds:
                scenario.stop(gds.process)
    if failures:
        sys.exit(f"cli: {len(failures)} difference(s) not accepted")
    print("cli: equal, apart from the accepted differences")


main()
