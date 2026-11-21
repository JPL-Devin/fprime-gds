#!/usr/bin/env python3
"""Run the scenario against the Python GDS and against fgds, and compare (section 6.1 of the design).

Usage: compare.py --fgds <binary> --ref <Ref binary> --dictionary <json> --python <fprime venv python> --out <dir>

Two comparisons, see README.md for the differences accepted in the first:
  live    each GDS drives its own run of Ref; what the scenario determines must be equal.
  replay  the bytes Ref sent in the fgds run are decoded by both; the output must be identical.
"""
import argparse
import json
import os
import subprocess
import sys

import scenario

HERE = os.path.dirname(os.path.abspath(__file__))
# Events of the command dispatcher: caused by the scenario's commands, in a fixed order.
SCENARIO_EVENTS = "CdhCore.cmdDisp."
# Counts the commands of the scenario; other channels depend on when the run was sampled.
SCENARIO_CHANNEL = "CdhCore.cmdDisp.CommandsDispatched"

parser = argparse.ArgumentParser()
for option in ("fgds", "ref", "dictionary", "python", "out"):
    parser.add_argument(f"--{option}", required=True)
args = parser.parse_args()
failures = []


def side(script, python, *arguments):
    subprocess.run([python, f"{HERE}/{script}", *arguments, "--dictionary", args.dictionary], check=True, cwd=HERE)


def load(path):
    with open(path) as file:
        return json.load(file)


def check(what, python, rust):
    same = python == rust
    print(f"  {'same' if same else 'DIFFERENT'}: {what}")
    if not same:
        failures.append(what)
        pairs = zip(python, rust) if isinstance(python, list) and len(python) == len(rust) else [(python, rust)]
        first = next((pair for pair in pairs if pair[0] != pair[1]))
        print(f"    python: {json.dumps(first[0])[:600]}\n    fgds:   {json.dumps(first[1])[:600]}")


def without_time(items):
    return [{key: value for key, value in item.items() if key != "time"} for item in items]


for name in ("python", "rust"):
    os.makedirs(f"{args.out}/{name}", exist_ok=True)

print("live: the scenario against Ref, once with each GDS")
side("python_gds.py", args.python, "live", "--ref", args.ref, "--out", f"{args.out}/python")
side("rust_gds.py", sys.executable, "live", "--fgds", args.fgds, "--ref", args.ref, "--out", f"{args.out}/rust")
python, rust = load(f"{args.out}/python/live.json"), load(f"{args.out}/rust/live.json")
assert len(python["commands"]) == len(scenario.COMMANDS), "the Python GDS did not record the commands"
check("sent commands (name, opcode, arguments)", python["commands"], rust["commands"])
uplinks = [[p.hex() for p in scenario.packets(open(f"{args.out}/{n}/uplink.bin", "rb").read())] for n in ("python", "rust")]
check("command packets on the link, byte for byte", *uplinks)
events = [without_time([e for e in run["events"] if e["name"].startswith(SCENARIO_EVENTS)]) for run in (python, rust)]
assert len(events[0]) >= 3 * len(scenario.COMMANDS), "the scenario's events are missing"
check(f"the {len(events[0])} events caused by the commands (all but time)", *events)
last = [[c for c in run["channels"] if c["name"] == SCENARIO_CHANNEL][-1] for run in (python, rust)]
check(f"last sample of {SCENARIO_CHANNEL} (all but time)", *without_time(last))
names = [sorted({c["name"] for c in run["channels"]}) for run in (python, rust)]
check(f"the set of {len(names[0])} channels received", *names)

print("replay: the bytes Ref sent in the fgds run, through both decoders")
capture = f"{args.out}/rust/downlink.bin"
side("python_gds.py", args.python, "replay", "--capture", capture, "--out", f"{args.out}/python/replay.json")
side("rust_gds.py", sys.executable, "replay", "--fgds", args.fgds, "--capture", capture,
     "--log-dir", f"{args.out}/rust", "--out", f"{args.out}/rust/replay.json")
python, rust = load(f"{args.out}/python/replay.json"), load(f"{args.out}/rust/replay.json")
assert python["events"] and python["channels"], "the capture holds no events or no channels"
check(f"{len(python['events'])} events (identifier, name, severity, time, arguments, text)", python["events"], rust["events"])
check(f"{len(python['channels'])} channel samples (identifier, name, time, value, text)", python["channels"], rust["channels"])

if failures:
    sys.exit(f"compare: {len(failures)} difference(s) not accepted")
print("compare: equal, apart from the accepted differences")
