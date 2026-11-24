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
import codecs
import fcntl
import json
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
import scenario  # noqa: E402

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


class Screen:
    """A process on a pseudo-terminal of ROWS by COLUMNS, and the screen it has drawn there.

    Knows what the program writes: text, cursor moves, erasing, and the switch to the alternate
    screen and back. Colours and other modes are read and dropped.
    """

    def __init__(self, command):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
        self.before = termios.tcgetattr(slave)
        self.process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=ENV, start_new_session=True)
        scenario.STARTED.append(self.process)
        os.close(slave)
        self.seen, self.done = "", 0
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.cells = [[" "] * COLUMNS for _ in range(ROWS)]
        self.row = self.column = 0
        self.modes = {}

    def read(self, patience=0.1):
        if select.select([self.master], [], [], patience)[0]:
            try:
                self.seen += self.decoder.decode(os.read(self.master, 65536))
            except OSError:  # the process closed its side
                pass
        # An escape sequence cut by the end of what was read waits for its rest.
        cut = re.search(r"\x1b(\[[?0-9;]*)?$", self.seen)
        whole = cut.start() if cut else len(self.seen)
        for sequence, private, numbers, action, char in re.findall(
                r"(\x1b\[(\??)([0-9;]*)([A-Za-z]))|([^\x1b]|\x1b[^\[])", self.seen[self.done:whole]):
            self.done += len(sequence or char)
            values = [int(n) for n in numbers.split(";") if n]
            first = values[0] if values else 1
            if char == "\r":
                self.column = 0
            elif char == "\n":
                self.row = min(ROWS - 1, self.row + 1)
            elif char == "\b":
                self.column = max(0, self.column - 1)
            elif char and char >= " " and len(char) == 1:
                if self.column < COLUMNS:
                    self.cells[self.row][self.column] = char
                    self.column += 1
            elif private and action in "hl":
                for mode in values:
                    self.modes[mode] = action == "h"
            elif action in "Hf":
                self.row = min(ROWS, first) - 1
                self.column = min(COLUMNS, values[1] if len(values) > 1 else 1) - 1
            elif action in "ABCDGd":
                row, column = {"A": (-first, 0), "B": (first, 0), "C": (0, first), "D": (0, -first)}.get(action, (0, 0))
                self.row = first - 1 if action == "d" else self.row + row
                self.column = first - 1 if action == "G" else self.column + column
                self.row, self.column = max(0, min(ROWS - 1, self.row)), max(0, min(COLUMNS - 1, self.column))
            elif action == "J":
                rows = range(ROWS) if not values or values[0] == 2 else range(self.row + 1, ROWS)
                for row in rows:
                    self.cells[row] = [" "] * COLUMNS
            elif action == "K":
                self.cells[self.row][self.column:] = [" "] * (COLUMNS - self.column)

    def type(self, keys):
        os.write(self.master, keys.encode())

    def rows(self):
        return ["".join(row) for row in self.cells]

    def text(self):
        return "\n".join(row.rstrip() for row in self.rows())

    def split(self):
        """The column of the border between the events pane and the channel table."""
        return self.rows()[0].index("┐")

    def events(self):
        """The inside of the events pane as one text: a line that takes two rows is whole in it."""
        return "".join(row[1:self.split()] for row in self.rows()[1:ROWS - 4])

    def channels(self):
        """The rows of the channel table."""
        return [row[self.split() + 2:COLUMNS - 1].rstrip() for row in self.rows()[1:ROWS - 4]]

    def channel(self, name):
        """The value shown for the channel `name`; None when it has no row."""
        for row in self.channels():
            if row.startswith(name + " "):
                return row.split()[1]
        return None

    def command_line(self):
        return self.rows()[ROWS - 2][1:COLUMNS - 1].rstrip()

    def expect(self, what, timeout=30.0):
        """What `what()` gives once it is true; the last of it after `timeout` seconds."""
        end = time.monotonic() + timeout
        while True:
            self.read()
            try:
                found = what()
            except ValueError:  # a screen without its borders yet
                found = None
            if found or time.monotonic() > end:
                return found


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
    screen = Screen([args.fgds, "tui", "--url", url])
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
