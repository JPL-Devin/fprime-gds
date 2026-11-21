#!/usr/bin/env python3
"""The command lines compared: `fprime-cli` of the Python GDS and the online subcommands of `fgds`.

Three parts (see README.md):
  definitions  `--list` of events, channels and commands, with and without filters
  live         `command-send` of the same commands against Ref; the packets on the link
  replay       the bytes Ref sent, fed to both ground systems while both command lines follow

Standard library only; the Python GDS is run through the executables of its virtual environment.
"""
import argparse
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

def event_definition(item):
    return {"id": item["id"], "name": item["name"], "severity": item["severity"].replace("EventSeverity.", ""),
            "time": item["raw_time"], "args": item["args"], "description": item["display_text"]}


def channel_definition(item):
    return {"id": item["id"], "name": item["name"], "time": item["raw_time"], "description": item["display_text"]}


def command_definitions(text):
    """The commands of the text form (the JSON form of fprime-cli is a Python `repr`, not JSON)."""
    found = []
    for line in text.splitlines():
        if match := re.fullmatch(r"(\S+) \((\d+)\) \| Takes (\d+) arguments\.", line):
            found.append({"name": match[1], "opcode": int(match[2]), "count": int(match[3]), "description": None, "args": []})
        elif line.startswith("Description: "):
            found[-1]["description"] = line[len("Description: "):]
        elif match := re.fullmatch(r"\t(\S+) \((\S+)\): (.*)", line):
            # fprime-cli names a primitive type by its Python class (`U32Type`), fgds as the dictionary does (`U32`).
            kind = re.sub(r"^([UI](8|16|32|64)|F(32|64)|Bool)Type$", r"\1", match[2])
            found[-1]["args"].append({"name": match[1], "type": "bool" if kind == "Bool" else kind, "description": match[3].strip()})
        elif line.strip():
            # A description of several lines continues the command's or its last argument's.
            holder = found[-1]["args"][-1] if found[-1]["args"] else found[-1]
            holder["description"] += "\n" + line.strip()
    return found


def definitions(fgds):
    print("definitions: the --list of both command lines")
    dictionary = ["--dictionary", args.dictionary]
    filters = [[], ["-i", "16777217", "16777223"], ["-c", "CdhCore.cmdDisp"], ["-s", "ommand"],
               ["-c", "CdhCore.cmdDisp", "Ref.pingRcvr", "-s", "Ping"]]
    for subcommand, shape in (("events", event_definition), ("channels", channel_definition)):
        for chosen in filters:
            _, python, _ = run([PYTHON_CLI, subcommand, "--list", "--json", *dictionary, *chosen])
            _, rust, _ = run(fgds.cli(subcommand, "--list", "--json", *chosen))
            python, rust = [shape(i) for i in json_lines(python)], [shape(i) for i in json_lines(rust)]
            assert python or chosen == filters[-1], f"fprime-cli {subcommand} --list {chosen} printed nothing"
            check(f"{len(python)} {subcommand[:-1]} definitions, filters {' '.join(chosen) or 'none'}", python, rust)
    for chosen in filters:
        _, python, _ = run([PYTHON_CLI, "command-send", "--list", *dictionary, *chosen])
        _, rust, _ = run(fgds.cli("commands", *chosen))
        python, rust = command_definitions(python), command_definitions(rust)
        assert python, f"fprime-cli command-send --list {chosen} printed nothing"
        check(f"{len(python)} command definitions, filters {' '.join(chosen) or 'none'}", python, rust)
    _, also, _ = run(fgds.cli("command-send", "--list"))
    _, rust, _ = run(fgds.cli("commands"))
    require("fgds command-send --list is fgds commands", also == rust and rust)


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
    code, _, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_NO_OPP"))
    require("an unknown command is refused and near names are proposed",
            code != 0 and "unknown command" in error and "CdhCore.cmdDisp.CMD_NO_OP" in error, error)
    code, _, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", "1"))
    require("a command with too few arguments is refused and its arguments are listed",
            code != 0 and "3 arguments" in error and "arg2 (F32)" in error, error)
    code, _, error = run(fgds.cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", "1", "x", "3"))
    require("a command with an argument of the wrong type is refused", code != 0 and "x" in error, error)
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
                    rust[subcommand] == history[subcommand] and rust[subcommand])
            subset(subcommand, [shape(i) for i in followed[subcommand]], [shape(i) for i in rust[subcommand]])
    finally:
        for gds in (fgds, python):
            if gds:
                scenario.stop(gds.process)
    if failures:
        sys.exit(f"cli: {len(failures)} difference(s) not accepted")
    print("cli: equal, apart from the accepted differences")


main()
