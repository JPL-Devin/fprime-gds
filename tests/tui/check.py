#!/usr/bin/env python3
"""`fgds tui` against Ref, typed into through a pseudo-terminal and read as a screen.

  the packet of a typed `CMD_NO_OP` is the packet of `fgds command-send`, and Ref answers it
  the command record and the events of the command are in the events pane
  the channel table holds the latest value of a channel: it changes with the next command
  a refused command puts nothing on the link and says why
  quit puts the terminal back: main screen, cursor shown, line mode and echo

The screen after the commands is kept in `<out>/screen.txt`. Standard library only.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import termios
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
import scenario  # noqa: E402
from screen import Screen  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--fgds", required=True)
parser.add_argument("--ref", required=True)
parser.add_argument("--dictionary", required=True)
parser.add_argument("--out", required=True)
args = parser.parse_args()
for name in ("fgds", "ref", "dictionary", "out"):
    setattr(args, name, os.path.abspath(getattr(args, name)))
os.makedirs(args.out, exist_ok=True)
failures = []
NO_OP = "CdhCore.cmdDisp.CMD_NO_OP"
COUNT = "CdhCore.cmdDisp.CommandsDispatched"
ROWS, COLUMNS = 45, 180
OFFERS = "┌ Tab ─"  # the top of the list of what Tab offers
ENV = {**os.environ, "TERM": "xterm", "NO_COLOR": "1"}


def require(what, condition, detail=""):
    print(f"  {'ok' if condition else 'FAILED'}: {what}")
    if not condition:
        failures.append(what)
        print(f"    {detail}"[:6000])


def packets(proxy, scid):
    return [p.hex() for p in scenario.uplink_packets(bytes(proxy.uplink), "ccsds", scid)]


def main():
    http, link = scenario.free_port(), scenario.free_port()
    url = f"http://127.0.0.1:{http}"
    scenario.start([args.fgds, "run", "--logs", f"{args.out}/logs", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}",
                    "--comm-tcp-address", f"127.0.0.1:{link}"], open(f"{args.out}/gds.log", "w"))
    scenario.wait_for("fgds", lambda: urllib.request.urlopen(f"{url}/api/v1/version", timeout=10).read())
    proxy = scenario.Proxy(link)
    scenario.start_ref(args.ref, proxy, args.out)
    scid = scenario.spacecraft_id(args.dictionary)
    with open(args.dictionary) as file:
        last = max(command["name"] for command in json.load(file)["commands"])

    before = len(packets(proxy, scid))
    scenario.wait_for("telemetry", lambda: subprocess.run([args.fgds, "channels", "--url", url, "-t", "30", "--json"],
                                                     capture_output=True, timeout=60).stdout)
    done = subprocess.run([args.fgds, "command-send", "--url", url, NO_OP], capture_output=True, text=True, timeout=60)
    assert done.returncode == 0, done.stdout + done.stderr
    reference = scenario.wait_for("the packet of command-send", lambda: packets(proxy, scid)[before:])
    time.sleep(scenario.SETTLE)  # the events of that command are over before the screen starts

    print("the screen on a pseudo-terminal")
    screen = Screen([args.fgds, "tui", "--url", url], ENV, ROWS, COLUMNS)
    try:
        titles = screen.expect(lambda: all(title in screen.text() for title in (" Events and commands ", " Channels: ", "fgds> ")))
        require("the three panes are drawn on the alternate screen", titles and screen.modes.get(1049), screen.text())
        sample = screen.expect(lambda: screen.channel("Ref.systemResources.MEMORY_USED"))
        require(f"the channel table fills from the stream: Ref.systemResources.MEMORY_USED is {sample} KB", sample, screen.text())

        # Tab completes the last part of the name as far as the two commands that have it share.
        before = len(packets(proxy, scid))
        screen.type("cmd CMD_NO\t")
        completed = screen.expect(lambda: screen.command_line() == f"fgds> cmd {NO_OP}")
        require("Tab completes CMD_NO to the name the offers share", completed, screen.text())
        require("and lists the offers over the panes", f"│{NO_OP}_STRING" in screen.text() and OFFERS in screen.text(), screen.text())
        screen.type("\r")
        record = rf"CMD   {re.escape(NO_OP)} \{{\}} by fgds-tui-\d+: sent, opcode \d+, packet ([0-9a-f]+)"
        shown = screen.expect(lambda: re.search(record, screen.events()))
        require("the command record is in the events pane", shown, screen.text())
        typed = scenario.wait_for("the packet of the typed command", lambda: packets(proxy, scid)[before:])
        require(f"the typed command is on the link as command-send puts it: {reference}", typed == reference, f"{typed}")
        require("the record shows that packet", shown and [shown.group(1)] == reference, f"{shown and shown.group(1)}")
        events = ("EVENT COMMAND CdhCore.cmdDisp.OpCodeDispatched: Opcode 0x", "EVENT ACTIVITY_HI CdhCore.cmdDisp.NoOpReceived: Received a NO-OP command",
                  "EVENT COMMAND CdhCore.cmdDisp.OpCodeCompleted: Opcode 0x")
        answered = screen.expect(lambda: all(event in screen.events() for event in events))
        require("Ref answers: OpCodeDispatched, NoOpReceived and OpCodeCompleted are in the events pane", answered, screen.text())
        require("the offers are gone and the command line is empty", OFFERS not in screen.text() and screen.command_line() == "fgds>", screen.text())

        first = screen.expect(lambda: screen.channel(COUNT))
        require(f"the channel table shows {COUNT}: {first}", first and first.isdigit(), screen.text())
        screen.type(f"cmd {NO_OP}\r")
        second = screen.expect(lambda: screen.channel(COUNT) not in (None, first) and screen.channel(COUNT))
        require(f"and its value after one more command, in the same row: {second}", first and second and int(second) == int(first) + 1,
                f"{first} {second}\n{screen.text()}")
        screen.expect(lambda: screen.events().count(events[2]) == 2)
        require("no channel sample is in the events pane", " CHAN " not in screen.events(), screen.text())
        with open(f"{args.out}/screen.txt", "w") as file:
            file.write(screen.text() + "\n")

        print("a command that is refused")
        time.sleep(scenario.SETTLE)
        before = len(packets(proxy, scid))
        screen.type(f"cmd {NO_OP} 1\r")
        refused = f"not sent: {NO_OP} takes 0 arguments, 1 given"
        require("a refused command says why", screen.expect(lambda: refused in screen.events()), screen.text())
        require("and how the command is typed", screen.expect(lambda: f"usage: cmd {NO_OP}" in screen.events()), screen.text())
        screen.type("cmd Ref.typeDemo.CHOICE PURPLE\r")
        value = 'not sent: argument choice: "PURPLE" is not a constant of enum Ref.Choice'
        require("a refused value says why", screen.expect(lambda: value in screen.events()), screen.text())
        time.sleep(scenario.SETTLE)
        require("the refused commands put nothing on the link", len(packets(proxy, scid)) == before, f"{packets(proxy, scid)[before:]}")

        print("scrollback and filters")
        screen.type("cmds\r")
        listed = screen.expect(lambda: f"{last} (0x" in screen.events())
        require(f"cmds lists the dictionary in the events pane, to its last command {last}", listed, screen.text())
        screen.type("\x1b[5~" * 60)  # Page Up, past the start
        back = screen.expect(lambda: "lines back" in screen.rows()[0] and f"fgds tui on {url}" in screen.events())
        require("Page Up goes back to the first line", back, screen.text())
        require("where the record of the first command is", re.search(record, screen.events()), screen.text())
        screen.type("\x1b[6~" * 60)  # Page Down
        follows = screen.expect(lambda: "lines back" not in screen.rows()[0] and f"{last} (0x" in screen.events())
        require("Page Down follows the stream again", follows, screen.text())
        screen.type("subscribe *.CommandsDispatched\r")
        only = screen.expect(lambda: " Channels: 1 of " in screen.rows()[0] and [row.split()[0] for row in screen.channels() if row] == [COUNT])
        require("a subscription leaves its channels in the table", only, screen.text())

        print("leaving")
        screen.type("quit\r")
        try:
            code = screen.process.wait(10)
        except subprocess.TimeoutExpired:
            code = None
        screen.read()
        require("quit leaves with exit code 0", code == 0, f"{code}")
        require("on the main screen, with the cursor shown", screen.modes.get(1049) is False and screen.modes.get(25) is True, f"{screen.modes}")
        after = termios.tcgetattr(screen.master)
        line_mode = after[3] & termios.ICANON and after[3] & termios.ECHO
        require("and the terminal in line mode with echo, as it was", line_mode and after[:4] == screen.before[:4], f"{screen.before} {after}")
    finally:
        screen.read()
        with open(f"{args.out}/terminal.raw", "w") as file:
            file.write(screen.seen)

    print("without a terminal")
    piped = subprocess.run([args.fgds, "tui", "--url", url], stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
    require("fgds tui stops and names fgds repl", piped.returncode != 0 and "needs a terminal" in piped.stderr and not piped.stdout,
            f"{piped.returncode} {piped.stdout} {piped.stderr}")


try:
    main()
finally:
    scenario.stop_all()
if failures:
    sys.exit(f"{len(failures)} checks failed: {failures}")
print("tui: every check passed")
