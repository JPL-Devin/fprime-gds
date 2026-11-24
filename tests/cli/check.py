#!/usr/bin/env python3
"""The command lines compared: `fprime-cli` of the Python GDS and the online subcommands of `fgds`.

Three parts (see README.md):
  definitions  `--list` of events, channels and commands, with and without filters
  live         `command-send` of the same commands against Ref; the packets on the link
  replay       the bytes Ref sent, fed to both ground systems while both command lines follow

Standard library only; the Python GDS is run through the executables of its virtual environment.
"""
import copy
import glob
import argparse
import difflib
import json
import hashlib
import shutil
import os
import re
import signal
import socket
import subprocess
import threading
import urllib.error
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

    def __init__(self, out, link=None, more=()):
        os.makedirs(out, exist_ok=True)
        http, self.link_port = scenario.free_port(), scenario.free_port()
        self.url = f"http://127.0.0.1:{http}"
        self.process = scenario.start(
            [args.fgds, "run", "--framing", args.framing, "--logs", f"{out}/logs", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}",
             *(["--comm-tcp-address", f"127.0.0.1:{self.link_port}"] if link is None else link), *more], open(f"{out}/gds.log", "w"))
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

    def __init__(self, out, zmq=False):
        os.makedirs(out, exist_ok=True)
        self.out = out
        self.link_port, tts = scenario.free_port(), scenario.free_port()
        # send-raw of fprime-cli reaches the link only through ZeroMQ (deviations.md); the sockets are files.
        self.sockets = [f"/tmp/fgds-cli-check-{os.getpid()}-{way}" for way in ("in", "out")] if zmq else []
        transport = ["--zmq-transport", *(f"ipc://{path}" for path in self.sockets)] if zmq else ["--no-zmq", "--tts-port", str(tts), "--tts-addr", "127.0.0.1"]
        self.shared = ["--dictionary", args.dictionary, *transport, "--file-storage-directory", f"{out}/files"]
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


# Round 9: the GDS options of fprime-cli, the same on `command-send`, `events` and `channels`, checked before
# anything is done. Each set of options goes through the six FORMS; both accept or both refuse, put the
# same packets on the link and, for a listing, print the same.
FORMS = [["command-send", NO_OP], ["command-send", "--list", "-s", "CMD_NO_OP"], ["events", "--list", "-s", "OpCode"],
         ["channels", "--list", "-s", "Commands"], ["events", "-t", "0.5"], ["channels", "-t", "0.5"]]
BAD = {  # one of each check, for the pairs: with two bad options the first check of fprime-cli decides
    "dictionary": ["--dictionary", "/nonexistent/x"], "set": ["--packet-set-name", "x"], "spec": ["--packet-spec", "/nonexistent/x"],
    "hash": ["--hash-file", "/nonexistent/x"], "files": ["--file-storage-directory", "/etc"], "logs": ["-l", "/etc"],
    "prefix": ["--log-prefix", "{out}/ro/p"],
}
STANDARD = [
    [],
    # absent, not readable, empty, valid
    ["--dictionary", "/nonexistent/x"], ["--dictionary", "{out}"], ["--dictionary", "{out}/unreadable.json"], ["--dictionary", ""],
    ["--dictionary", "{out}/not-json"], ["--dictionary", "{out}/list.json"], ["--dictionary", "{out}/empty.json"],
    ["--dictionary", "{out}/nocmd.json"], ["--dictionary", "/nonexistent/x", "--dictionary", "{dictionary}"],
    ["--packet-spec", "/nonexistent/x"], ["--packet-spec", "{out}"], ["--packet-spec", ""], ["--packet-spec", "{out}/not-json"],
    ["--packet-spec", "{out}/p.xml"],
    ["--packet-set-name", "x"], ["--packet-set-name", ""], ["--packet-set-name", "RefPackets"],
    ["--hash-file", "/nonexistent/x"], ["--hash-file", "{out}/ro/h"], ["--hash-file", ""], ["--hash-file", "{out}"], ["--hash-file", "{out}/not-json"],
    ["--file-storage-directory", "/nonexistent/x"], ["--file-storage-directory", "{out}/not-json"], ["--file-storage-directory", "{out}/ro"],
    ["--file-storage-directory", "{out}/ro/sub"], ["--file-storage-directory", "/etc"], ["--file-storage-directory", ""],
    ["--file-storage-directory", "{out}/files"],
    ["-l", "/nonexistent/x"], ["-l", "{out}/not-json"], ["-l", "{out}/ro"], ["-l", "{out}/ro/sub"], ["-l", "/etc"], ["-l", ""],
    ["-l", "{out}/logs"], ["-l", "/nonexistent/x", "-l", "{out}/logs"], ["-l", "{out}/ro", "--log-directly"], ["-l", "{out}/logs", "--log-directly"],
    ["-l", "", "--log-directly"],
    ["--log-prefix", "/nonexistent/x"], ["--log-prefix", "{out}/ro/p"], ["--log-prefix", "/etc/hostname"], ["--log-prefix", ""], ["--log-prefix", "p"],
    ["--log-prefix", "p/"],
    # with `--dictionary` a deployment is not looked at; without, both end with 255 (main)
    ["--dictionary", "{dictionary}", "-d", "/nonexistent/x"], ["-d", ""], ["--dictionary", "{dictionary}", "-d", "{out}"],
    # options without a use in fgds, with values argparse accepts and refuses
    ["--log-to-stdout"], ["--log-directly"], ["--disable-data-logging"], ["--no-clear-history"], ["--log-level-gds", "DEBUG"],
    ["--log-level-gds", "x"], ["--tts-port", "x"], ["--file-uplink-cooldown", "nan"], ["--file-uplink-cooldown", "x"],
    ["--file-uplink-chunk-size", "0"], ["--file-uplink-chunk-size", "1.5"], ["--zmq-transport", "a"], ["-l"], ["--hash-file"],
]
# Two bad options: both tools refuse; which one each reports is its own (deviations.md).
PAIRS = [BAD[a] + BAD[b] for a in BAD for b in BAD if a != b]
PAIRED = [(["--dictionary", "{dictionary}"] if "--dictionary" not in pair else []) + pair for pair in PAIRS]
STANDARD += PAIRED
EMPTY_HASH = ["--hash-file", ""]
# The lines fprime-cli prints for its own logging (deviations.md).
LOGGING = [["--log-to-stdout"]]


def standard(out):
    """STANDARD through FORMS as command lines [subcommand, token...], the things they name made."""
    os.makedirs(f"{out}/ro", exist_ok=True)
    os.chmod(f"{out}/ro", 0o555)
    for name, text in (("not-json", "x\n"), ("empty.json", "{}\n"), ("list.json", "[1, 2]\n"), ("unreadable.json", "{}\n"),
                       ("p.xml", "<packets name='x' namespace='y' size='100'></packets>\n")):
        if not os.path.exists(f"{out}/{name}"):
            open(f"{out}/{name}", "w").write(text)
    os.chmod(f"{out}/unreadable.json", 0)
    dictionary = json.load(open(args.dictionary))
    dictionary["commands"] = [c for c in dictionary["commands"] if c["name"] != NO_OP]
    dictionary["events"], dictionary["telemetryChannels"] = dictionary["events"][1:], dictionary["telemetryChannels"][1:]
    json.dump(dictionary, open(f"{out}/nocmd.json", "w"))
    return [(form + [token.format(out=out, dictionary=args.dictionary) for token in options], options, form)
            for options in STANDARD for form in (FORMS[::2] if options in PAIRED else FORMS)]


def compare_standard(lines, python, rust):
    ended = {}
    for (tokens, options, form), expected, got in zip(lines, python, rust):
        what = f"{' '.join(form)} {' '.join(options)}"
        listing, following = "--list" in form, "-t" in form
        same = got["exit"] == expected["exit"] and got["packets"] == expected["packets"]
        ended[expected["exit"]] = ended.get(expected["exit"], 0) + 1
        if options == EMPTY_HASH:
            require(f"{what}: fprime-cli takes the empty text as no hash file (exit code 0), fgds refuses it, nothing sent, nothing printed",
                    expected["exit"] == 0 and got["exit"] not in (0, 2) and not got["packets"] and not got["stdout"], f"{expected} {got}")
        elif expected["exit"] == 2:
            require(f"{what}: both end with exit code 2, nothing sent, nothing printed",
                    same and not got["packets"] and not got["stdout"] and got["message"] == expected["message"], f"{expected} {got}")
        elif expected["exit"] != 0:  # which of two bad options is reported, and the exit code, are each tool's own
            require(f"{what}: both refuse (fprime-cli exit code {expected['exit']}, fgds {got['exit']}), nothing sent, nothing printed",
                    got["exit"] not in (0, 2) and not got["packets"] + expected["packets"] and not got["stdout"], f"{expected} {got}")
        elif following:
            require(f"{what}: both end with exit code 0", same and not got["packets"], f"{expected} {got}")
        elif options in LOGGING:
            require(f"{what}: exit code 0, the same packets; fprime-cli also prints the lines of its logging",
                    same and "Logging system initialized!" in expected["stdout"] and got["stdout"] in expected["stdout"], f"{expected} {got}")
        else:
            check(f"{what}: exit 0, {len(expected['packets'])} packet(s), {len(expected['stdout'].splitlines())} line(s)", expected, got)
            if listing and "nocmd.json" not in " ".join(options):
                require(f"{what}: the listing is not empty", bool(got["stdout"]))
    require(f"the option matrix has lines of every outcome: {ended}", all(ended.get(code, 0) >= 20 for code in (0, 1, 2)))


# `send-raw`: bytes sent as one packet. {packet} is the packet of CMD_NO_OP, {out} holds the files.
RAW_SENT = [  # both send the same packet
    ["--hex-string", "{packet}"], ["--hex-string", "0x{packet}"], ["--hex-string", "{PACKET}"], ["--hex-string", "{spaced}"],
    ["--hex", "{packet}"], ["--hex-string={packet}"], ["--verbose", "--hex-string", "{packet}"], ["--bin-path", "{out}/no-op.bin"],
    ["--bin", "{out}/no-op.bin", "--verbose"],
    ["--hex-string", "{packet}", "--dictionary", "{dictionary}"], ["--hex-string", "{packet}", "--log-to-stdout"],
    ["--hex-string", "{packet}", "-l", "{out}/logs"], ["--hex-string", "{packet}", "--log-prefix", "{out}/prefix"],
]
RAW_REFUSED = [  # both refuse, nothing sent
    [], ["--verbose"], ["--hex-string"], ["--bin-path"], ["--hex-string", "{packet}", "--bin-path", "{out}/no-op.bin"],
    ["--hex-string", "zz"], ["--hex-string", "abc"], ["--hex-string", ""], ["--hex-string", "-1"],
    ["--bin-path", "{out}/absent.bin"], ["--bin-path", "{out}"], ["--bin-path", ""], ["{packet}"],
    ["--hex-string", "{packet}", "--arguments"], ["--hex-string", "{packet}", "--list"],
    ["--hex-string", "{packet}", "-l", "/etc"], ["--hex-string", "{packet}", "--file-storage-directory", "/etc"],
    ["--hex-string", "{packet}", "--dictionary", "/nonexistent.json"], ["--hex-string", "{packet}", "--hash-file", "/nonexistent/x"],
    ["--hex-string", "{packet}", "--packet-set-name", "x"], ["--hex-string", "{packet}", "--packet-spec", "/nonexistent.xml"],
]
# fprime-cli ends with exit code 0, fgds refuses (deviations.md): no bytes at all (an empty file, `0x`
# alone); `0x` taken out of the middle of the text.
RAW_PYTHON_ONLY = [["--bin-path", "{out}/empty.bin"], ["--hex-string", "0x"], ["--hex-string", "100x20"]]
# Bytes that are no packet of the CCSDS framing (the first two name no packet type): fgds refuses them;
# fprime-cli ends with exit code 0, nothing reaches the link, and the uplink thread of the Python GDS ends
# with TypeRangeException, so that nothing is sent afterwards (deviations.md). These lines are the last ones.
RAW_NO_PACKET = [["--hex-string", "deadbeef"], ["--hex-string", "{packet}"]]


def raw(out, packet):
    """The command lines of send-raw, the files they name made."""
    os.makedirs(out, exist_ok=True)
    open(f"{out}/no-op.bin", "wb").write(bytes.fromhex(packet))
    open(f"{out}/empty.bin", "wb").close()
    spaced = " ".join(packet[at:at + 2] for at in range(0, len(packet), 2))
    return [["send-raw"] + [t.format(out=out, packet=packet, PACKET=packet.upper(), spaced=spaced, dictionary=args.dictionary) for t in tokens]
            for tokens in RAW_SENT + RAW_REFUSED + RAW_PYTHON_ONLY + RAW_NO_PACKET]


def compare_raw(lines, python, rust):
    json.dump([lines, python, rust], open(f"{args.out}/raw.json", "w"), indent=1)
    for at, (tokens, expected, got) in enumerate(zip(lines, python, rust)):
        what = " ".join(tokens)
        if at < len(RAW_SENT):
            require(f"{what}: both end with exit code 0 and put the same packet on the link",
                    expected["exit"] == got["exit"] == 0 and len(got["packets"]) == 1 and got["packets"] == expected["packets"], f"{expected} {got}")
        elif at < len(RAW_SENT) + len(RAW_REFUSED):
            require(f"{what}: both refuse (fprime-cli exit code {expected['exit']}, fgds {got['exit']}), nothing sent",
                    expected["exit"] != 0 and got["exit"] != 0 and (expected["exit"] == 2) == (got["exit"] == 2)
                    and not got["packets"] + expected["packets"], f"{expected} {got}")
        elif at < len(lines) - len(RAW_NO_PACKET):
            require(f"{what}: fprime-cli ends with exit code 0, fgds refuses and sends nothing",
                    expected["exit"] == 0 and got["exit"] not in (0, 2) and not got["packets"], f"{expected} {got}")
    if args.framing == "ccsds":
        (what, packet), (bad, after), (refused, sent) = [" ".join(line) for line in lines[-2:]], python[-2:], rust[-2:]
        require(f"{what}: fgds refuses and sends nothing, and sends the packet of the next line", refused["exit"] not in (0, 2)
                and not refused["packets"] and sent["exit"] == 0 and len(sent["packets"]) == 1, f"{refused} {sent}")
        require(f"{what}: fprime-cli ends with exit code 0 with nothing on the link, and the Python GDS sends nothing afterwards ({packet})",
                bad["exit"] == after["exit"] == 0 and not bad["packets"] + after["packets"], f"{bad} {after}")


# `file-uplink` with the GDS options of fprime-cli (round 9): the same table as `command-send`.
UPLINK_SENT = [  # both uplink the file: the same packets on the link
    [], ["-t", "30"], ["--hash-file", "{out}/small.bin"], ["--file-storage-directory", "{out}/files"], ["-l", "{out}/logs"],
    ["--log-prefix", "p"], ["--packet-set-name", "RefPackets"], ["--dictionary", "{dictionary}"], ["--dictionary", "{dictionary}", "-d", "/nonexistent"],
    ["--log-level-gds", "DEBUG", "--no-clear-history", "--disable-data-logging", "--file-uplink-cooldown", "0.5"], ["--time", "30"],
    ["--file-uplink-chunk-size", "100"], ["--file-uplink-chunk-size", "100", "--file-uplink-cooldown", "1"],
]
UPLINK_REFUSED = [  # both refuse, nothing sent
    *BAD.values(), ["--dictionary", "{out}/small.bin"], ["-l"], ["--log-level-gds", "x"], ["--tts-port", "x"], ["--arguments"], ["--l", "x"],
]


def uplinks(out):
    """The command lines of file-uplink, the file they send made."""
    os.makedirs(f"{out}/ro", exist_ok=True)
    os.chmod(f"{out}/ro", 0o555)
    open(f"{out}/small.bin", "wb").write(bytes(range(200)))
    lines = [["file-uplink", f"{out}/small.bin", f"cli-uplink-{at}.bin"] + [t.format(out=out, dictionary=args.dictionary) for t in tokens]
             for at, tokens in enumerate(UPLINK_SENT + UPLINK_REFUSED)]
    return lines + [["file-uplink"], ["file-uplink", f"{out}/absent.bin"]]


def compare_uplinks(lines, python, rust):
    json.dump([lines, python, rust], open(f"{args.out}/uplinks.json", "w"), indent=1)
    for at, (tokens, expected, got) in enumerate(zip(lines, python, rust)):
        what = " ".join(tokens)
        if at < len(UPLINK_SENT):
            # The start packet names the copy each GDS keeps of the file: compared from the destination on.
            destination = tokens[2].encode().hex()
            starts = [packets[0].endswith(destination) for packets in (expected["packets"], got["packets"]) if packets]
            require(f"{what}: both end with exit code 0 and put the same {len(expected['packets'])} packets on the link",
                    expected["exit"] == got["exit"] == 0 and len(got["packets"]) >= 3 and starts == [True, True]
                    and got["packets"][1:] == expected["packets"][1:], f"{expected} {got}")
            if "--file-uplink-chunk-size" in tokens:
                sizes = [len(packet) // 2 for packet in got["packets"]]
                require(f"{what}: the 200 bytes go in two data packets of 100 bytes ({sizes} bytes a packet), as with fprime-cli",
                        len(sizes) == 4 and sizes[1] == sizes[2] and [len(p) for p in got["packets"][1:]] == [len(p) for p in expected["packets"][1:]], sizes)
        else:
            require(f"{what}: both refuse (fprime-cli exit code {expected['exit']}, fgds {got['exit']}), nothing sent",
                    expected["exit"] != 0 and got["exit"] != 0 and (expected["exit"] == 2) == (got["exit"] == 2)
                    and not got["packets"] + expected["packets"], f"{expected} {got}")


def dashes(name, gds, out, lines=None, subcommand="command-send"):
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
            code, output, error = run(gds.cli(*([subcommand] if subcommand else []), *tokens))
            if subcommand or tokens[:2] == ["command-send", NO_OP]:  # a packet of another line would be counted with the next one
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
            "time": item["raw_time"], "args": scenario.plain(item["args"]),
            "display_text": item["display_text"]}


def skeleton(value):
    """The shape of a JSON value: names, nesting and the type of each leaf."""
    if isinstance(value, dict):
        return {key: skeleton(item) for key, item in value.items()}
    if isinstance(value, list):
        return [skeleton(item) for item in value]
    return "number" if isinstance(value, (int, float)) and not isinstance(value, bool) else type(value).__name__


def shapes(python, rust):
    """`events --json`, field by field: an event of fgds has the names, nesting and leaf types of the same event of fprime-cli."""
    ours = {(item["id"], item["raw_time"]): item for item in rust}
    pairs = [(item, ours[item["id"], item["raw_time"]]) for item in python if (item["id"], item["raw_time"]) in ours]
    differ = [(theirs, mine) for theirs, mine in pairs if skeleton(theirs) != skeleton(mine) or list(theirs) != list(mine)]
    counts = {n: sum(1 for theirs, _ in pairs if min(len(theirs["args"]), 2) == n) for n in (0, 1, 2)}
    leaves = sorted({json.dumps(skeleton(arg)) for theirs, _ in pairs for arg in theirs["args"]})
    require(f"events --json: the {len(pairs)} events both printed have the same field names, order, nesting and value types "
            f"({counts[0]} with no argument, {counts[1]} with one, {counts[2]} with several; argument shapes {', '.join(leaves)})",
            pairs and not differ and all(counts.values()), f"{counts} " + json.dumps(differ[:2]))


def received_channel(item):
    return {"id": item["id"], "name": item["name"], "time": item["raw_time"], "display_text": str(item["display_text"])}


def subset(what, python, rust):
    """Every item of fprime-cli is an item of fgds (README.md: fprime-cli skips items and orders by time)."""
    # README.md, accepted difference 1: an item fprime-cli printed twice counts once.
    missing = [item for item in python if item not in rust]
    require(f"{what}: each of the {len(python)} items fprime-cli printed is among the {len(rust)} of fgds, same values",
            python and not missing, f"not printed by fgds: {json.dumps(missing[:2])}\n    fgds has e.g.: {json.dumps(rust[:2])}")


def offline(out):
    """seqgen, prm-write and prm-decode with the dictionary options of the Python tools: same accept or refuse, same file written."""
    os.makedirs(out, exist_ok=True)
    deployment = os.path.dirname(os.path.dirname(args.dictionary))
    with open(f"{out}/a.seq", "w") as file:
        file.write("R00:00:00 CdhCore.cmdDisp.CMD_NO_OP\nR00:00:01 Ref.typeDemo.CHOICE ONE\n")
    with open(f"{out}/p.json", "w") as file:
        file.write('{"Ref.typeDemo": {"CHOICE_PRM": "TWO"}}')
    run([args.fgds, "prm-write", "dat", f"{out}/p.json", "-d", args.dictionary, "-o", f"{out}/x.dat"])
    given, absent = ["--dictionary", args.dictionary], "/nonexistent/x"
    seqgen = [(given, True), (["-d", deployment], True), (["-d", deployment] + given, True), ([f"-d{deployment}"], True),
              (given + ["--packet-set-name", "RefPackets"], True), (given + ["-t", "2"], True), (["--dict", args.dictionary], True),
              (["-d", absent], False), (["--dictionary", absent], False), (given + ["--packet-set-name", "x"], False),
              (given + ["--packet-spec", absent], False), (["--d", args.dictionary], False), (given + ["--hash-file", "x"], False),
              ([], False)]
    short = [(["-d", args.dictionary], True), (given, True), ([f"-d{args.dictionary}"], True), (["-d", absent], False), ([], False)]
    cases = [("fprime-seqgen", "seqgen", [], "a.seq", options, None, sent) for options, sent in seqgen]
    for options, sent in short:
        cases += [("fprime-prm-write", "prm-write", [kind], "p.json", options, "-o", sent) for kind in ("dat", "seq")]
        cases += [("fprime-prm-decode", "prm-decode", form, "x.dat", options, "-o", sent) for form in ([], ["-f", "text"], ["-f", "csv"])]
    for tool, subcommand, first, source, options, flag, written in cases:
        results = []
        for name, command in (("python", [f"{args.venv_bin}/{tool}"]), ("rust", [args.fgds, subcommand])):
            path = f"{out}/{name}.out"
            if os.path.exists(path):
                os.remove(path)
            code, _, _ = run(command + first + [f"{out}/{source}"] + ([flag, path] if flag else [path]) + options, cwd="/")
            content = None
            if os.path.exists(path):
                with open(path, "rb") as file:
                    content = file.read()
            results.append((code == 0, content))
        what = f"{subcommand} {' '.join(first + options)}".replace(deployment, "<deployment>")
        if written:
            require(f"{what}: both write the same file", results[0] == results[1] and results[0][0] and results[0][1], results)
        else:
            require(f"{what}: both refuse and write nothing", results[0] == results[1] == (False, None), results)


def merges(out):
    """merge-dictionary on variants of the Ref dictionary and the dictionaries of the repository: same accept or refuse, same file."""
    os.makedirs(out, exist_ok=True)
    with open(args.dictionary) as file:
        ref = json.load(file)

    def variant(name, change):
        data = copy.deepcopy(ref)
        change(data)
        with open(f"{out}/{name}.json", "w") as file:
            json.dump(data, file)
        return f"{out}/{name}.json"

    def other(data):  # another deployment: same names at other identifiers, one new channel, a text that is not ASCII
        data["metadata"]["deploymentName"] = "Other.Top"
        for section, key in (("commands", "opcode"), ("events", "id"), ("telemetryChannels", "id"), ("parameters", "id")):
            for entry in data[section][:5]:
                entry[key] += 0x70000000
        data["telemetryChannels"].append(dict(data["telemetryChannels"][0], name="Other.new", id=0x7FFF0000, annotation="d\u00e9j\u00e0 \"vu\"\n"))
        data["constants"].append({"kind": "constant", "qualifiedName": "Other.ratio", "type": {"name": "F64", "kind": "float", "size": 64}, "value": 1e-07})
        data["extra"] = {"limit": 1.5, "empty": [], "none": None}

    def changed(data):  # same identifier, another definition; same identifier, another name; another type definition
        data["metadata"]["deploymentName"] = "Changed"
        data["commands"][0]["annotation"] = "changed"
        data["telemetryChannels"][1]["name"] = "Changed.renamed"
        data["telemetryChannels"][2]["annotation"] = "changed"
        data["typeDefinitions"][0]["annotation"] = "changed"

    def version(data):
        data["metadata"]["deploymentName"] = "Versioned"
        data["metadata"]["frameworkVersion"] = "v0"
        data["metadata"]["libraryVersions"] = ["x"]

    paths = {"ref": args.dictionary, "other": variant("other", other), "changed": variant("changed", changed), "version": variant("version", version),
             "unnamed": variant("unnamed", lambda data: data["metadata"].update(deploymentName="9 lives")),
             "no-sets": variant("no-sets", lambda data: data.pop("telemetryPacketSets")),
             "twice": variant("twice", lambda data: data["events"].append(data["events"][0])),
             "bad-id": variant("bad-id", lambda data: data["commands"][0].update(opcode=True)),
             "list": variant("list", lambda data: (data.clear(), data.update(metadata=[]))), "absent": f"{out}/absent.json"}
    with open(f"{out}/text.json", "w") as file:
        file.write("not JSON")
    paths["text"] = f"{out}/text.json"
    repository = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    found = sorted(glob.glob(f"{repository}/fixtures/**/*ictionary*.json", recursive=True))
    cases = [(first, second, options) for first, second in (("ref", "ref"), ("ref", "other"), ("other", "ref"), ("ref", "changed"), ("ref", "version"),
                                                           ("unnamed", "other"), ("ref", "no-sets"), ("ref", "twice"), ("bad-id", "ref"),
                                                           ("list", "ref"), ("ref", "absent"), ("text", "ref"), ("other", "changed"))
             for options in ([], ["--prefer-primary"], ["--permissive"], ["--no-namespace"], ["--namespace-all"], ["--name", "A.b"],
                             ["--name", "9x"], ["--prefix", "A", "--prefix", "B.c"], ["--prefer-primary", "--no-namespace", "--permissive"],
                             ["--prefer-primary", "--namespace-all", "--permissive"])]
    cases += [("ref", "other", options) for options in (["--prefix", "A"], ["--prefix", "A", "--prefix", "A"], ["--prefix", "A", "--prefix", "9"],
                                                        ["--no-namespace", "--namespace-all"], ["--no-namespace", "--prefix", "A", "--prefix", "B"],
                                                        ["--perm"], ["--p"], ["--output"], ["--unknown"])]
    cases += [("ref", "ref", ["extra"]), ("ref", None, [])]
    for one in found:
        paths[one] = one
        cases += [(one, other_path, options) for other_path in found + ["ref"] for options in ([], ["--prefer-primary", "--permissive"])]
    written = 0
    for first, second, options in cases:
        results = []
        for name, command in (("python", [f"{args.venv_bin}/fprime-merge-dictionary"]), ("rust", [args.fgds, "merge-dictionary"])):
            path = f"{out}/{name}.out"
            if os.path.exists(path):
                os.remove(path)
            code, _, _ = run(command + [paths[first]] + ([paths[second]] if second else []) + options + ([] if "--output" in options else ["--output", path]), cwd=out)
            content = None
            if os.path.exists(path):
                with open(path, "rb") as file:
                    content = file.read()
            results.append((code == 0, content))
        written += results[0][0]
        short = lambda path: os.path.relpath(path, repository) if path and path.startswith(repository) else path
        require(f"merge-dictionary {short(first)} {short(second)} {' '.join(options)}: both {'write the same file' if results[0][0] else 'refuse and write nothing'}",
                results[0] == results[1] and (results[0][1] is None) != results[0][0], [(ok, content and len(content)) for ok, content in results])
    require(f"merge-dictionary: {written} of {len(cases)} command lines write a file", written >= 20, written)


# 7. The ways `fgds run` reaches the flight software and serves its clients, each with Ref: what `fprime-gds` does
# with `--ip-client`, `--comm-adapter uart`, `--gui-port` and `-g none`.

class Relay:
    """Ref (a TCP client) on one side; on the other a pseudo-terminal, a port the GDS connects to, or a pair of
    UDP ports (`serial="udp"`: the GDS sends to `address` and receives on `recv_port`)."""

    def __init__(self, serial):
        self.uplink, self.downlink = bytearray(), bytearray()
        self.server = socket.create_server(("127.0.0.1", 0))
        self.port = self.server.getsockname()[1]
        if serial == "udp":
            self.udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            self.udp.bind(("127.0.0.1", 0))
            self.address = f"127.0.0.1:{self.udp.getsockname()[1]}"
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as free:
                free.bind(("127.0.0.1", 0))
                self.recv_port = free.getsockname()[1]
        elif serial:
            self.far, near = os.openpty()
            self.device = os.ttyname(near)
            os.close(near)
        else:
            self.listener = socket.create_server(("127.0.0.1", 0))
            self.address = f"127.0.0.1:{self.listener.getsockname()[1]}"
        threading.Thread(target=self._run, args=(serial,), daemon=True).start()

    def _run(self, serial):
        ref, _ = self.server.accept()
        if serial == "udp":
            read = lambda: self.udp.recv(65535)
            write = lambda data: self.udp.sendto(data, ("127.0.0.1", self.recv_port))
        elif serial:
            def write(data):  # a pseudo-terminal takes what the reader has room for
                while data:
                    data = data[os.write(self.far, data):]
            read = lambda: os.read(self.far, 4096)
        else:
            gds, _ = self.listener.accept()
            read, write = (lambda: gds.recv(4096)), gds.sendall
        threading.Thread(target=self._pump, args=(read, ref.sendall, self.uplink), daemon=True).start()
        self._pump(lambda: ref.recv(4096), write, self.downlink)

    @staticmethod
    def _pump(read, write, log):
        try:
            while data := read():
                log.extend(data)
                write(data)
        except OSError:
            pass


def capabilities(out):
    scid = scenario.spacecraft_id(args.dictionary)
    ways = [
        ("the TCP server (the default)", lambda: (None, None), []),
        ("--comm tcp-client", lambda: (Relay(False), None), []),
        ("--comm uart over a pseudo-terminal", lambda: (Relay(True), None), []),
        ("--comm udp", lambda: (Relay("udp"), None), []),
        ("--no-page", lambda: (None, None), ["--no-page"]),
    ]
    sent = {}
    for index, (name, make, more) in enumerate(ways):
        relay, _ = make()
        if relay is None:
            link = None
        elif hasattr(relay, "device"):
            link = ["--comm", "uart", "--comm-uart-device", relay.device, "--comm-uart-baud", "115200"]
        elif hasattr(relay, "udp"):
            link = ["--comm", "udp", "--comm-udp-address", relay.address, "--comm-udp-recv-port", str(relay.recv_port)]
        else:
            link = ["--comm", "tcp-client", "--comm-tcp-address", relay.address]
        gds = ref = None
        try:
            gds = Fgds(f"{out}/{index}", link, more)
            if relay is None:
                relay = scenario.Proxy(gds.link_port)
            ref = scenario.start_ref(args.ref, relay, f"{out}/{index}")
            code, sample, error = run(gds.cli("channels", "-t", "30", "--json"))
            require(f"fgds run with {name}: telemetry of Ref arrives", bool(json_lines(sample)), f"{code} {error}")
            expected = run(gds.cli("command-send", NO_OP, "--dry-run"))[1].strip()
            code, output, error = run(gds.send(NO_OP, []))
            scenario.wait_for("the event of the command", lambda: any(
                "NoOpReceived" in json.dumps(item) for item in json_lines(run(gds.cli("events", "--history", "--no-follow", "--json"))[1])))
            sent[name] = [p.hex() for p in scenario.uplink_packets(bytes(relay.uplink), args.framing, scid)]
            require(f"fgds run with {name}: the command packet on the link is the one of `--dry-run`, and Ref answers NoOpReceived",
                    code == 0 and sent[name] == [expected], f"{sent[name]} {expected} {error}")
            page = urllib.request.Request(gds.url + "/")
            try:
                status = urllib.request.urlopen(page, timeout=10).status
            except urllib.error.HTTPError as refused:
                status = refused.code
            wanted = 404 if "--no-page" in more else 200
            require(f"fgds run with {name}: the page answers {wanted}", status == wanted, status)
            if hasattr(relay, "device"):
                peer = gds.get("/status")
                lost = open(f"{out}/{index}/gds.log").read().count("received sequence count")
                require("no packet of Ref is lost on the serial device", lost <= 1, f"{lost} gaps in the sequence counts")
                require("the link names the device and the rate", "115200bps" in json.dumps(peer), json.dumps(peer)[:600])
        finally:
            for process in (ref, gds and gds.process):
                if process:
                    scenario.stop(process)
    done = run([args.fgds, "run", "--dictionary", args.dictionary, "--logs", f"{out}/refused", "--bind", "127.0.0.1:0", "--comm", "uart", "--comm-uart-baud", "12345"])
    require("fgds run --comm uart --comm-uart-baud 12345: refused when it starts", done[0] != 0 and "is not one of" in done[2], done)
    done = run([args.fgds, "run", "--dictionary", args.dictionary, "--logs", f"{out}/refused", "--bind", "127.0.0.1:0", "--comm", "udp", "--comm-udp-recv-port", "0"])
    require("fgds run --comm udp --comm-udp-recv-port 0: refused when it starts", done[0] != 0 and "is not a port" in done[2], done)


# Round 9, F3: no option is accepted and ignored. For every subcommand that is not `run` or `check`, every long
# option any subcommand or the fprime-cli table has is tried: it is refused as unknown, refused as an option
# of `fgds run`, or it has a line below: two command lines that differ by this option alone and whose outcomes
# (exit code, standard output, files written, command packets on the link, more than 2.5 s taken) must differ.
# NO_EFFECT are the options of fprime-cli about facilities fgds does not have (deviations.md): accepted, as
# fprime-cli accepts them; those with a value are in VALUE_CHECKED and must refuse a bad one.
ONLINE = ["events", "channels", "commands", "command-send", "send-raw", "file-uplink", "files upload", "files queue", "files pause",
          "files resume", "files cancel", "files list", "files download"]
OFFLINE = ["seqgen", "prm-write dat", "prm-write seq", "prm-decode", "merge-dictionary", "dp decode", "dp validate"]
TABLE = ["events", "channels", "command-send", "send-raw", "file-uplink"]
NO_EFFECT = ["--no-zmq", "--log-to-stdout", "--disable-data-logging", "--no-clear-history"]
VALUE_CHECKED = {"--zmq-transport": (["a", "b"], ["a"]), "--tts-port": (["1"], ["x"]), "--tts-addr": (["a"], []),
                 "--log-level-gds": (["DEBUG"], ["x"]), "--file-uplink-cooldown": (["1"], ["x"]), "--file-uplink-chunk-size": (["1"], ["x"])}
NAMED = {  # the options of the fprime-cli table that name something: there, not there
    "--dictionary": (["{D}"], ["/nonexistent/x"]), "--logs": (["{out}/logs"], ["/etc"]), "--hash-file": (["{out}/s.bin"], ["/nonexistent/x"]),
    "--file-storage-directory": (["{out}/files"], ["/proc/x/y"]), "--log-prefix": (["p"], ["/nonexistent/p"]),
    "--packet-spec": ([], ["--packet-spec", "/nonexistent/x"]), "--packet-set-name": (["RefPackets"], ["x"]),
    "--deployment": ([], ["--deployment", "/nonexistent/x"]), "--log-directly": (["-l", "{out}/logs"], ["-l", "", "--log-directly"]),
}
LISTING = {"--ids": (["--ids", "1"], []), "--components": (["-c", "nothing"], []), "--search": (["-s", "NO_OP"], []), "--json": (["--json"], [])}
FOLLOWED = {**{name: (["--list"] + a, ["--list"] + b) for name, (a, b) in LISTING.items()}, "--list": (["--list"], ["--no-follow", "--history"]),
            "--history": (["--no-follow", "--history"], ["--no-follow"]), "--no-follow": (["--no-follow"], ["-t", "4"]),
            "--timeout": (["-t", "1"], ["-t", "4"])}
SEQ, PRM, DAT, FDP = "{out}/a.seq", "{out}/p.json", "{out}/x.dat", "{fixtures}/dp/test_dp_data/DpDemoUncompressed.fdp"
MERGE = ["{out}/one.json", "{out}/two.json", "--output", "o.json"]
EFFECT = {
    "events": FOLLOWED, "channels": FOLLOWED, "commands": LISTING,
    "command-send": {**{name: ([NO_OP, "--list"] + a, [NO_OP, "--list"] + b) for name, (a, b) in LISTING.items()},
                     "--list": ([NO_OP, "--list"], [NO_OP]), "--dry-run": ([NO_OP, "--dry-run"], [NO_OP]),
                     "--arguments": ([NO_OP + "_STRING", "--arguments", "a"], [NO_OP + "_STRING", "--arguments", "abc"])},
    "send-raw": {"--hex-string": (["--hex-string", "{P}"], []), "--bin-path": (["--bin-path", "{out}/p.bin"], []),
                 "--verbose": (["--hex-string", "{P}", "--verbose"], ["--hex-string", "{P}"])},
    "file-uplink": {"--timeout": (["{out}/s.bin", "a", "-t", "0"], ["{out}/s.bin", "a"]), "--no-wait": (["{out}/s.bin", "a", "--no-wait"], ["{out}/s.bin", "a"]),
                    "--file-uplink-chunk-size": (["{out}/s.bin", "a", "--file-uplink-chunk-size", "100"], ["{out}/s.bin", "a"]),
                    "--file-uplink-cooldown": (["{out}/s.bin", "a", "--file-uplink-cooldown", "1.5"], ["{out}/s.bin", "a"])},
    "files upload": {"--destination": (["{out}/s.bin", "-d", "/tmp/fgds-effects-y"], ["{out}/s.bin"]), "--timeout": (["{out}/s.bin", "-t", "0"], ["{out}/s.bin"]),
                     "--no-wait": (["{out}/s.bin", "--no-wait"], ["{out}/s.bin"])},
    "files queue": {}, "files pause": {}, "files resume": {}, "files cancel": {}, "files list": {},
    "files download": {"--output": (["h.txt", "-o", "one"], ["h.txt", "-o", "other"])},
    "seqgen": {"--dictionary": ([SEQ, "o.bin", "--dictionary", "{D}"], [SEQ, "o.bin", "--dictionary", "/nonexistent/x"]),
               "--deployment": ([SEQ, "o.bin", "-d", "{deployment}"], [SEQ, "o.bin", "-d", "/nonexistent/x"]),
               "--packet-spec": ([SEQ, "o.bin", "--dictionary", "{D}"], [SEQ, "o.bin", "--dictionary", "{D}", "--packet-spec", "/nonexistent/x"]),
               "--packet-set-name": ([SEQ, "o.bin", "--dictionary", "{D}"], [SEQ, "o.bin", "--dictionary", "{D}", "--packet-set-name", "x"]),
               "--timebase": ([SEQ, "o.bin", "--dictionary", "{D}", "-t", "2"], [SEQ, "o.bin", "--dictionary", "{D}"]),
               "--all-errors": (["{out}/bad.seq", "o.bin", "--dictionary", "{D}", "--all-errors"], ["{out}/bad.seq", "o.bin", "--dictionary", "{D}"])},
    "prm-decode": {"--dictionary": ([DAT, "-o", "o", "--dictionary", "{D}"], [DAT, "-o", "o", "--dictionary", "/nonexistent/x"]),
                   "--format": ([DAT, "-o", "o", "-d", "{D}", "-f", "csv"], [DAT, "-o", "o", "-d", "{D}"]),
                   "--output": ([DAT, "-o", "o", "-d", "{D}"], [DAT, "-o", "other", "-d", "{D}"])},
    "merge-dictionary": {"--output": (MERGE, MERGE[:2] + ["--output", "other.json"]), "--name": (MERGE + ["--name", "n"], MERGE),
                         "--prefix": (MERGE + ["--prefix", "a", "b"], MERGE), "--permissive": (MERGE[:1] + ["{out}/version.json"] + MERGE[2:] + ["--permissive"], MERGE[:1] + ["{out}/version.json"] + MERGE[2:]),
                         "--prefer-primary": (MERGE[:1] + ["{out}/changed.json"] + MERGE[2:] + ["--permissive", "--prefer-primary"],
                                              MERGE[:1] + ["{out}/changed.json"] + MERGE[2:] + ["--permissive"]),
                         "--no-namespace": (MERGE[:1] + ["{out}/ids.json"] + MERGE[2:] + ["--no-namespace"], MERGE[:1] + ["{out}/ids.json"] + MERGE[2:]),
                         "--namespace-all": (MERGE[:1] + ["{out}/ids.json"] + MERGE[2:] + ["--namespace-all"], MERGE[:1] + ["{out}/ids.json"] + MERGE[2:])},
    "dp decode": {"--bin-file": (["-b", FDP, "-d", "{dp}", "-o", "o.json"], ["-d", "{dp}", "-o", "o.json"]),
                  "--bin-dir": (["-B", "{out}/dp", "-d", "{dp}"], ["-d", "{dp}"]),
                  "--dictionary": (["-b", FDP, "-d", "{dp}", "-o", "o.json"], ["-b", FDP, "-d", "/nonexistent/x", "-o", "o.json"]),
                  "--output": (["-b", FDP, "-d", "{dp}", "-o", "o.json"], ["-b", FDP, "-d", "{dp}", "-o", "other.json"]),
                  "--rename-id": (["-B", "{out}/dp", "-d", "{dp}", "-r"], ["-B", "{out}/dp", "-d", "{dp}"]),
                  "--disable-decompression": (["-b", FDP.replace("Unc", "C"), "-d", "{dp}", "-o", "o.json", "-z"], ["-b", FDP.replace("Unc", "C"), "-d", "{dp}", "-o", "o.json"]),
                  "--data-products-max-bytes": (["-b", FDP, "-d", "{dp}", "-o", "o.json", "--data-products-max-bytes", "8"], ["-b", FDP, "-d", "{dp}", "-o", "o.json"])},
    "dp validate": {"--bin-file": (["-b", FDP, "-d", "{dp}"], ["-d", "{dp}"]), "--dictionary": (["-b", FDP, "-d", "{dp}"], ["-b", FDP, "-d", "/nonexistent/x"]),
                    "--header-size": (["-b", FDP, "-s", "3"], ["-b", FDP, "-s", "40"]), "--guess-size": (["-b", FDP, "-g"], ["-b", FDP, "-s", "3"]),
                    "--verbose": (["-b", FDP, "-d", "{dp}", "-v"], ["-b", FDP, "-d", "{dp}"])},
}
for kind in ("dat", "seq"):
    EFFECT[f"prm-write {kind}"] = {
        "--dictionary": ([PRM, "-o", "o", "--dictionary", "{D}"], [PRM, "-o", "o", "--dictionary", "/nonexistent/x"]),
        "--defaults": ([PRM, "-o", "o", "-d", "{D}", "--defaults"], [PRM, "-o", "o", "-d", "{D}"]),
        "--output": ([PRM, "-o", "o", "-d", "{D}"], [PRM, "-o", "other", "-d", "{D}"])}
EFFECT["prm-write seq"]["--save"] = ([PRM, "-o", "o", "-d", "{D}", "--save"], [PRM, "-o", "o", "-d", "{D}"])
BASE = {"files cancel": ["nothing"], "files download": ["h.txt"], "events": ["--list"], "channels": ["--list"], "command-send": [NO_OP, "--list"], "send-raw": ["--hex-string", "{P}"], "file-uplink": ["{out}/s.bin", "a"]}
for name in TABLE:
    for option, (there, absent) in NAMED.items():
        EFFECT[name].setdefault(option, (BASE[name] + ([option] if there and not there[0].startswith("-") else []) + there,
                                         BASE[name] + ([option] if absent and not absent[0].startswith("-") else []) + absent))
    for option, (good, bad) in VALUE_CHECKED.items():
        EFFECT[name].setdefault(option, (BASE[name] + [option] + good, BASE[name] + [option] + bad))


def effects(out):
    os.makedirs(f"{out}/work", exist_ok=True)
    fixtures = os.path.join(os.path.dirname(os.path.dirname(HERE)), "fixtures")
    for name, text in (("a.seq", "R00:00:00 CdhCore.cmdDisp.CMD_NO_OP\n"), ("bad.seq", "R00:00:00 No.such\nR00:00:01 Nor.this\n"),
                       ("p.json", '{"Ref.typeDemo": {"CHOICE_PRM": "TWO"}}')):
        open(f"{out}/{name}", "w").write(text)
    open(f"{out}/s.bin", "wb").write(bytes(range(250)))
    run([args.fgds, "prm-write", "dat", f"{out}/p.json", "-d", args.dictionary, "-o", f"{out}/x.dat"])
    data = json.load(open(args.dictionary))
    for name, change in (("one", lambda d: d), ("two", lambda d: d["metadata"].update(deploymentName="Two")),
                         ("changed", lambda d: d["commands"][0].update(annotation="changed")),
                         ("version", lambda d: d["metadata"].update(deploymentName="Two", frameworkVersion="other")),
                         ("ids", lambda d: (d["metadata"].update(deploymentName="Two"), d["commands"][0].update(opcode=d["commands"][0]["opcode"] + 99999)))):
        copy = json.loads(json.dumps(data))
        change(copy)
        json.dump(copy, open(f"{out}/{name}.json", "w"))
    os.makedirs(f"{out}/dp", exist_ok=True)
    for name in ("DpDemoCompressed.fdp", "DpDemoUncompressed.fdp"):
        shutil.copy(f"{fixtures}/dp/test_dp_data/{name}", f"{out}/dp/{name}")
    fgds = Fgds(f"{out}/gds", more=["--files-cooldown", "0"])
    ref, scid = None, scenario.spacecraft_id(args.dictionary)
    try:
        proxy = scenario.Proxy(fgds.link_port)
        ref = scenario.start_ref(args.ref, proxy, f"{out}/gds")
        assert json_lines(run(fgds.cli("channels", "-t", "30", "--json"))[1]), "no telemetry within 30 s"
        packet = run(fgds.cli("command-send", NO_OP, "--dry-run"))[1].strip()
        open(f"{out}/p.bin", "wb").write(bytes.fromhex(packet))
        open(f"{out}/gds/h-source.txt", "w").write("downlinked\n")
        run(fgds.cli("command-send", "FileHandling.fileDownlink.SendFile", "--arguments", "h-source.txt", "h.txt"))
        time.sleep(3)
        places = {"out": out, "D": args.dictionary, "P": packet, "fixtures": fixtures, "dp": f"{fixtures}/dp/test_dp_data/dictionary_ref.json",
                  "deployment": os.path.dirname(os.path.dirname(args.dictionary))}
        bind = fgds.url.split("//")[1]
        for at, key in enumerate(("bind", "dictionary")):
            for good in (True, False):
                value = ([bind, "127.0.0.1:1"] if at == 0 else [args.dictionary, "/nonexistent/x"])[not good]
                open(f"{out}/{key}-{good}.toml", "w").write(f'{key} = "{value}"\n')

        def outcome(subcommand, tokens, online):
            for name in os.listdir(f"{out}/work"):
                os.remove(f"{out}/work/{name}")
            before, started = len(scenario.uplink_packets(bytes(proxy.uplink), args.framing, scid)), time.time()
            url = [] if not online or "--bind" in tokens or "--config" in tokens else ["--url", fgds.url]
            code, output, error = run([args.fgds, *subcommand.split(), *url, *(t.format(**places) for t in tokens)], cwd=f"{out}/work")
            slow = time.time() - started > 2.5
            time.sleep(0 if not online else 1.5 if "upl" in subcommand else 0.3)
            packets = [len(p) for p in scenario.uplink_packets(bytes(proxy.uplink), args.framing, scid)[before:]]
            files = {name: hashlib.sha256(open(f"{out}/work/{name}", "rb").read()).hexdigest() for name in sorted(os.listdir(f"{out}/work"))}
            return {"exit": code, "stdout": output, "stderr": error, "files": files, "packets": packets, "slow": slow}

        for cooldown in ("0", "1"):
            before, started = len(scenario.uplink_packets(bytes(proxy.uplink), args.framing, scid)), time.time()
            code = run(fgds.cli("file-uplink", f"{out}/s.bin", f"cooldown-{cooldown}", "--file-uplink-chunk-size", "50", "--file-uplink-cooldown", cooldown))[0]
            taken, sent = time.time() - started, len(scenario.uplink_packets(bytes(proxy.uplink), args.framing, scid)) - before
            require(f"file-uplink --file-uplink-chunk-size 50 --file-uplink-cooldown {cooldown}: 250 bytes in 5 data packets (7 packets), "
                    f"{'at least 5 s' if cooldown == '1' else 'less than 2 s'} ({taken:.1f} s)",
                    code == 0 and sent == 7 and (taken >= 5 if cooldown == "1" else taken < 2), (code, sent, taken))
        helps = {s: set(re.findall(r"^\s+(?:-\w, )?(--[a-z-]+)", run([args.fgds, *s.split(), "--help"])[1], re.M)) for s in ONLINE + OFFLINE + ["run"]}
        universe = sorted(set().union(*helps.values()) | set(LONG)) 
        counts = {"unknown": 0, "of fgds run": 0, "effect": 0, "listed": 0}
        for subcommand in ONLINE + OFFLINE:
            online, table = subcommand in ONLINE, EFFECT[subcommand]
            base = BASE.get(subcommand, next(iter(table.values()))[0] if table else [])
            for option in universe:
                if option in ("--help", "--arguments") and option not in table:
                    continue  # the help text; `--arguments` of command-send takes what follows it
                what = f"fgds {subcommand} {option}"
                if option == "--url" and online or option == "--bind" and online or option == "--config":
                    key = "dictionary" if not online else "bind"
                    rest = [t for t in base if t not in ("--dictionary", "-d", "{D}")] if not online else base
                    pair = {"--url": (base, base + ["--url", "http://127.0.0.1:1"]), "--bind": (base + ["--bind", bind], base + ["--bind", "127.0.0.1:1"]),
                            "--config": (rest + ["--config", f"{out}/{key}-True.toml"], rest + ["--config", f"{out}/{key}-False.toml"])}[option]
                    if option == "--config" and subcommand in ("merge-dictionary", "dp decode", "dp validate"):
                        pair = (base, base + ["--config", "/nonexistent/x"])  # the file is read: one that is not there is refused
                elif option in table:
                    pair = table[option]
                elif option in NO_EFFECT and subcommand in TABLE:
                    result = run([args.fgds, *subcommand.split(), "--url", fgds.url, *(t.format(**places) for t in base), option])
                    require(f"{what}: accepted as fprime-cli accepts it, an option of a facility fgds does not have (deviations.md)", result[0] == 0, result)
                    counts["listed"] += 1
                    continue
                else:
                    line = [args.fgds, *subcommand.split(), *(["--url", fgds.url] if online else []), *(t.format(**places) for t in base), option]
                    code, _, error = run(line, cwd=f"{out}/work")
                    if "a value is required" in error or "expected one argument" in error:
                        code, _, error = run(line + ["x"], cwd=f"{out}/work")
                    known = f"--{option[2:]} is an option of" in error
                    unknown = any(text in error for text in (f"unexpected argument '{option}'", "unrecognized arguments: " + option))
                    counts["of fgds run" if known else "unknown"] += 1
                    if not (code == 2 and (known or unknown)):
                        require(f"{what}: refused, or has a line in EFFECT", False, f"{code} {error}")
                    continue
                first, second = (outcome(subcommand, tokens, online) for tokens in pair)
                counts["effect"] += 1
                if first == second:
                    require(f"{what}: `{' '.join(pair[0])}` and `{' '.join(pair[1])}` differ in what they do", False, first)
        require(f"no option accepted and ignored: {len(ONLINE + OFFLINE)} subcommands x {len(universe)} options; {counts['effect']} change what the "
                f"subcommand does (two command lines compared), {counts['of fgds run']} are refused as options of `fgds run`, {counts['unknown']} as "
                f"unknown, {counts['listed']} are options of fprime-cli without an effect in fgds", all(counts.values()), counts)
    finally:
        if ref:
            scenario.stop(ref)
        scenario.stop(fgds.process)


def main():
    os.makedirs(args.out, exist_ok=True)
    print("every option of every subcommand: an effect, or refused")
    effects(f"{args.out}/effects")
    if os.environ.get("CLI_CHECK_ONLY") == "effects":
        return
    print("fgds run: TCP server, TCP client, serial device, UDP, without the page")
    capabilities(f"{args.out}/capabilities")
    print("offline tools with the dictionary options")
    offline(f"{args.out}/offline")
    print("merge-dictionary through both")
    merges(f"{args.out}/merge")
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
        lines = standard(f"{args.out}/standard")
        print(f"the GDS options of fprime-cli: {len(STANDARD)} sets of options through {len(FORMS)} forms of command-send, events and channels, through both against Ref")
        tokens = [line[0] for line in lines]
        fgds = Fgds(f"{args.out}/rust-standard")
        rust_standard = dashes("fgds", fgds, f"{args.out}/rust-standard", tokens, None)
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python-standard")
        compare_standard(lines, dashes("fprime-cli", python, f"{args.out}/python-standard", tokens, None), rust_standard)
        scenario.stop(python.process)
        lines = uplinks(f"{args.out}/uplinks")
        print(f"file-uplink with the GDS options: {len(lines)} command lines through both against Ref")
        fgds = Fgds(f"{args.out}/rust-uplinks")
        rust_uplinks = dashes("fgds", fgds, f"{args.out}/rust-uplinks", lines, None)
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python-uplinks")
        compare_uplinks(lines, dashes("fprime-cli", python, f"{args.out}/python-uplinks", lines, None), rust_uplinks)
        scenario.stop(python.process)
        fgds = Fgds(f"{args.out}/rust-raw")
        packet = run(fgds.cli("command-send", NO_OP, "--dry-run"))[1].strip()
        lines = raw(f"{args.out}/raw", packet)
        print(f"send-raw: {len(lines)} command lines through both against Ref")
        rust_raw = dashes("fgds", fgds, f"{args.out}/rust-raw", lines, None)
        scenario.stop(fgds.process)
        python = PythonGds(f"{args.out}/python-raw", zmq=True)
        compare_raw(lines, dashes("fprime-cli", python, f"{args.out}/python-raw", lines, None), rust_raw)
        scenario.stop(python.process)
        for path in python.sockets:
            if os.path.exists(path):
                os.remove(path)
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
        shapes(followed["events"], rust["events"])
    finally:
        for gds in (fgds, python):
            if gds:
                scenario.stop(gds.process)
    if failures:
        sys.exit(f"cli: {len(failures)} difference(s) not accepted")
    print("cli: equal, apart from the accepted differences")


main()
