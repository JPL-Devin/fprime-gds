#!/usr/bin/env python3
"""Compare fgds with the Python GDS (section 6.1 of the design). See README.md.

Usage: compare.py --fgds <binary> --dictionary <json> --python <fprime venv python> --out <dir>
                  [--ref <Ref binary> --framing ccsds|fprime]

With --ref, two comparisons against a running Ref built with the given framing:
  live       each GDS drives its own run of Ref; what the scenario determines must be equal.
  replay     the bytes Ref sent in the fgds run are decoded by both; the output must be identical.
Without --ref:
  synthetic  every event and channel of the dictionary, built by the Python encoders with boundary
             values, decoded by both; the output must be identical.
"""
import argparse
import json
import os
import re
import subprocess
import sys

import scenario

HERE = os.path.dirname(os.path.abspath(__file__))
# Counts the commands of the scenario; other channels depend on when the run was sampled.
SCENARIO_CHANNEL = "CdhCore.cmdDisp.CommandsDispatched"
OPCODE = re.compile(r"Opcode 0x[0-9a-f]+")
# Set by the scenario's commands and by nothing else.
SCENARIO_CHANNELS = "Ref.typeDemo."

parser = argparse.ArgumentParser()
for option in ("fgds", "dictionary", "python", "out"):
    parser.add_argument(f"--{option}", required=True)
parser.add_argument("--ref")
parser.add_argument("--framing", choices=["ccsds", "fprime"], default="ccsds")
args = parser.parse_args()
shapes = scenario.Shapes(args.dictionary)
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
        pairs = list(zip(python, rust)) if isinstance(python, list) and len(python) == len(rust) else [(python, rust)]
        different = [pair for pair in pairs if pair[0] != pair[1]]
        if len(pairs) > 1:
            names = sorted({pair[0].get("name", "") for pair in different if isinstance(pair[0], dict)})
            print(f"    {len(different)} of {len(pairs)} items differ" + (f", of {len(names)} names: {', '.join(names)[:400]}" if names else ""))
        print(f"    python: {json.dumps(different[0][0])[:600]}\n    fgds:   {json.dumps(different[0][1])[:600]}")


def only(items, *keys):
    return [{key: item[key] for key in keys} for item in items]


def decoded(what, python, rust):
    """Both decodings of one capture: identical, and how much of the dictionary they cover."""
    assert python["events"] and python["channels"], "the capture holds no events or no channels"
    check(f"{what}: {len(python['events'])} events (identifier, name, severity, time, argument names and values, text)",
          python["events"], rust["events"])
    check(f"{what}: {len(python['channels'])} channel samples (identifier, name, time, value, text)",
          python["channels"], rust["channels"])
    shapes.coverage("events", shapes.events, [e["id"] for e in rust["events"]], failures)
    shapes.coverage("channels", shapes.channels, [c["id"] for c in rust["channels"]], failures)


def replay(capture, framing, name):
    side("python_gds.py", args.python, "replay", "--framing", framing, "--capture", capture, "--out", f"{args.out}/python/{name}.json")
    side("rust_gds.py", sys.executable, "replay", "--framing", framing, "--fgds", args.fgds, "--capture", capture,
         "--log-dir", f"{args.out}/rust", "--out", f"{args.out}/rust/{name}.json")
    return load(f"{args.out}/python/{name}.json"), load(f"{args.out}/rust/{name}.json")


def live():
    print(f"live ({args.framing} framing): the scenario against Ref, once with each GDS")
    for script, python, binary in (("python_gds.py", args.python, []), ("rust_gds.py", sys.executable, ["--fgds", args.fgds])):
        out = f"{args.out}/{'rust' if binary else 'python'}"
        side(script, python, "live", "--framing", args.framing, "--ref", args.ref, "--out", out, *binary)
    python, rust = load(f"{args.out}/python/live.json"), load(f"{args.out}/rust/live.json")
    assert len(python["commands"]) == len(scenario.COMMANDS), "the Python GDS did not record the commands"
    check(f"the {len(scenario.COMMANDS)} sent commands (name, opcode, argument names and values)", python["commands"], rust["commands"])
    shapes.coverage("commands", shapes.commands, [c["name"] for c in rust["commands"]], failures)
    scid = shapes.constants.get("ComCfg.SpacecraftId", 0x44)
    uplinks = [scenario.uplink(open(f"{args.out}/{n}/uplink.bin", "rb").read(), args.framing, scid) for n in ("python", "rust")]
    assert len(uplinks[0]) == len(scenario.COMMANDS), f"{len(uplinks[0])} commands on the link of the Python GDS"
    check(f"the {'TC frames' if args.framing == 'ccsds' else 'command packets'} on the link, byte for byte", *uplinks)
    refused = [[a["status"] // 100 for a in run["answers"]] for run in (python, rust)]
    assert refused[0] == [2] * len(scenario.COMMANDS) + [4] * len(scenario.REJECTED), python["answers"]
    check(f"accepted and refused requests ({len(scenario.REJECTED)} refused, none of them sent)", *refused)
    # The command dispatcher and the commanded components run in their own threads: the order
    # between two components is not determined, the order within one is.
    components = sorted({name.rsplit(".", 1)[0] for name, _ in scenario.COMMANDS})
    for component in components:
        events = [only([e for e in run["events"] if e["name"].startswith(component + ".")], "id", "name", "severity", "display_text")
                  for run in (python, rust)]
        assert events[0], f"no event of {component}"
        # A command may complete after the next one is dispatched (the health component answers
        # on its next cycle): the dispatcher's events are compared command by command.
        events = [sorted(run, key=lambda e: (OPCODE.search(e["display_text"]) or [""])[0]) for run in events]
        check(f"the {len(events[0])} events of {component} (identifier, name, severity, text)", *events)
    errors = [e["display_text"] for e in python["events"] if e["name"].endswith("OpCodeError")]
    assert len(errors) == 1, errors
    print(f"    the command that fails on board: {errors[0]!r}")
    last = [{c["name"]: {k: c[k] for k in ("id", "value", "display_text")} for c in run["channels"]
             if c["name"] == SCENARIO_CHANNEL or c["name"].startswith(SCENARIO_CHANNELS)} for run in (python, rust)]
    assert SCENARIO_CHANNEL in last[0] and len(last[0]) > 1, sorted(last[0])
    check(f"last sample of {SCENARIO_CHANNEL} and of {len(last[0]) - 1} channels of {SCENARIO_CHANNELS[:-1]} (value, text)", *last)
    names = [sorted({c["name"] for c in run["channels"]}) for run in (python, rust)]
    check(f"the set of {len(names[0])} channels received", *names)

    print(f"replay ({args.framing} framing): the bytes Ref sent in the fgds run, through both decoders")
    capture = f"{args.out}/rust/downlink-whole.bin"
    with open(capture, "wb") as file:
        file.write(scenario.whole_frames(open(f"{args.out}/rust/downlink.bin", "rb").read(), args.framing,
                                         shapes.constants.get("ComCfg.TmFrameFixedSize", 1024)))
    decoded("replay", *replay(capture, args.framing, "replay"))


def synthetic():
    print("synthetic: every event and channel of the dictionary with boundary values, through both decoders")
    capture = f"{args.out}/synthetic.bin"
    side("python_gds.py", args.python, "synthetic", "--capture", capture, "--out", f"{args.out}/python/synthetic.json")
    side("rust_gds.py", sys.executable, "replay", "--framing", "fprime", "--fgds", args.fgds, "--capture", capture,
         "--log-dir", f"{args.out}/rust", "--out", f"{args.out}/rust/synthetic.json")
    python, rust = load(f"{args.out}/python/synthetic.json"), load(f"{args.out}/rust/synthetic.json")
    decoded("synthetic", python, rust)
    for what, table in (("events", shapes.events), ("channels", shapes.channels)):
        missing = set(table) - {item["id"] for item in rust[what]}
        if missing:
            failures.append(f"synthetic: {len(missing)} {what} not decoded")


for name in ("python", "rust"):
    os.makedirs(f"{args.out}/{name}", exist_ok=True)
live() if args.ref else synthetic()
if failures:
    sys.exit(f"compare: {len(failures)} difference(s) not accepted:\n  " + "\n  ".join(failures))
print("compare: equal, apart from the accepted differences" if args.ref else "compare: equal")
