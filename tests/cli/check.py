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
