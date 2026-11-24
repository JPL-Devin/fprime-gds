#!/usr/bin/env python3
"""`fgds repl` against Ref, typed into through a pseudo-terminal.

  the packet of a typed `CMD_NO_OP` is the packet of `fgds command-send`, and Ref answers it
  the command record and the events of the command are printed, and the prompt is drawn again under them
  an event that arrives while a line is being typed is printed above that line
  a refused command puts nothing on the link and says why
  without a terminal on the standard input, the stream is printed and nothing is read

What the terminal received is kept in `<out>/transcript.txt`. Standard library only.
"""
import argparse
import os
import pty
import re
import select
import signal
import subprocess
import sys
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
PROMPT = "fgds> "
# HOME: the history file of the prompt goes with the other output.
ENV = {**os.environ, "HOME": args.out, "TERM": "xterm", "NO_COLOR": "1"}


def require(what, condition, detail=""):
    print(f"  {'ok' if condition else 'FAILED'}: {what}")
    if not condition:
        failures.append(what)
        print(f"    {detail}"[:3000])


class Terminal:
    """A process on a pseudo-terminal, and everything it wrote there."""

    def __init__(self, command):
        self.master, slave = pty.openpty()
        self.process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=ENV, start_new_session=True)
        scenario.STARTED.append(self.process)
        os.close(slave)
        self.seen = ""

    def read(self, patience=0.1):
        if select.select([self.master], [], [], patience)[0]:
            try:
                self.seen += os.read(self.master, 65536).decode(errors="replace")
            except OSError:  # the process closed its side
                pass

    def type(self, keys):
        os.write(self.master, keys.encode())

    def expect(self, pattern, start=0, timeout=30.0):
        """The first match of `pattern` at or after `start`; None after `timeout` seconds."""
        end = time.monotonic() + timeout
        while True:
            if found := re.search(pattern, self.seen[start:]):
                return start + found.start(), start + found.end()
            if time.monotonic() > end:
                return None
            self.read()

    def rows(self):
        """The rows of the terminal after everything written: what a person sees, scrolled off rows included.

        Knows what the editor writes: carriage return, line feed, cursor left and right, clear to the
        end of the row or of the screen. Rows are taken as wide as needed.
        """
        rows, row, column = [[]], 0, 0
        for escape, count, action, char in re.findall(r"(\x1b\[\??([0-9;]*)([A-Za-z]))|([^\x1b])", self.seen):
            if char == "\r":
                column = 0
            elif char == "\n":
                row += 1
                if row == len(rows):
                    rows.append([])
            elif char and char >= " ":
                rows[row][column:column + 1] = char
                column += 1
            elif action in "CD":
                column = max(0, column + int(count or 1) * (1 if action == "C" else -1))
            elif action in "KJ":
                del rows[row][column:]
                if action == "J":
                    del rows[row + 1:]
            rows[row].extend(" " * (column - len(rows[row])))
        return ["".join(cells).rstrip() for cells in rows]


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
    send = [args.fgds, "command-send", "--url", url, NO_OP]

    def command_send():
        before = len(packets(proxy, scid))
        done = subprocess.run(send, capture_output=True, text=True, timeout=60)
        assert done.returncode == 0, done.stdout + done.stderr
        return scenario.wait_for("the packet of command-send", lambda: packets(proxy, scid)[before:])

    scenario.wait_for("telemetry", lambda: subprocess.run([args.fgds, "channels", "--url", url, "-t", "30", "--json"],
                                                     capture_output=True, timeout=60).stdout)
    reference = command_send()
    time.sleep(scenario.SETTLE)  # the events of that command are over before the prompt starts

    print("the prompt on a pseudo-terminal")
    terminal = Terminal([args.fgds, "repl", "--url", url])
    try:
        require("the prompt is drawn", terminal.expect(re.escape(PROMPT)), terminal.seen)
        terminal.type("mute channels\r")
        require("mute channels is acknowledged", terminal.expect("muted: channels"), terminal.seen)

        # Tab completes the last part of the name to the full name of the command.
        before, at = len(packets(proxy, scid)), len(terminal.seen)
        terminal.type("cmd CMD_NO\t")
        require("Tab completes CMD_NO to the full name", terminal.expect(re.escape(f"cmd {NO_OP}"), at), terminal.seen[at:])
        terminal.type("\r")
        record = terminal.expect(rf"CMD   {re.escape(NO_OP)} \{{\}} by fgds-repl-\d+: sent, opcode \d+, packet [0-9a-f]+", at)
        require("the command record is printed", record, terminal.seen[at:])
        typed = scenario.wait_for("the packet of the typed command", lambda: packets(proxy, scid)[before:])
        require(f"the typed command is on the link as command-send puts it: {reference}", typed == reference, f"{typed}")
        received = terminal.expect(r"EVENT COMMAND CdhCore\.cmdDisp\.OpCodeDispatched: [^\r\n]*", at)
        completed = terminal.expect(r"EVENT COMMAND CdhCore\.cmdDisp\.OpCodeCompleted: [^\r\n]*", at)
        no_op = terminal.expect(r"EVENT ACTIVITY_HI CdhCore\.cmdDisp\.NoOpReceived: [^\r\n]*", at)
        require("Ref answers: OpCodeDispatched, NoOpReceived and OpCodeCompleted are printed",
                received and completed and no_op, terminal.seen[at:])
        last = max(found[1] for found in (received, completed, no_op) if found)
        require("the prompt is drawn again under the events", terminal.expect(re.escape(PROMPT), last, 5.0), repr(terminal.seen[at:]))

        # An event while a line is being typed: it is printed, then the line is drawn again under it.
        at = len(terminal.seen)
        terminal.type("filt")
        require("the typed text is echoed", terminal.expect("filt", at), terminal.seen[at:])
        at = len(terminal.seen)
        also = command_send()
        other = terminal.expect(rf"CMD   {re.escape(NO_OP)} \{{\}} by [^\r\n]*: sent", at)
        event = terminal.expect(r"EVENT ACTIVITY_HI CdhCore\.cmdDisp\.NoOpReceived", at)
        require("the command of another client and its event are printed", other and event and also == reference, terminal.seen[at:])
        terminal.expect(re.escape(PROMPT + "filt"), event[1] if event else at, 5.0)
        rows = terminal.rows()
        require("above the line being typed, which is the last row and is whole",
                rows[-1] == PROMPT + "filt" and any("NoOpReceived" in row for row in rows[-4:-1]), "\n".join(rows[-6:]))
        terminal.type("ers\r")
        require("the line is still whole: `filters` answers", terminal.expect("muted: all channels", at), terminal.seen[at:])

        # A refused command: the reason of the GDS, and nothing on the link.
        before, at = len(packets(proxy, scid)), len(terminal.seen)
        terminal.type(f"cmd {NO_OP} 1\r")
        require("a refused command says why", terminal.expect(rf"not sent: {re.escape(NO_OP)} takes 0 arguments, 1 given", at), terminal.seen[at:])
        terminal.type("cmd Ref.typeDemo.CHOICE PURPLE\r")
        require("a refused value says why", terminal.expect(r"not sent: [^\r\n]*PURPLE", at), terminal.seen[at:])
        terminal.type("cmd Ref.nope.CMD\r")
        require("an unknown command says so", terminal.expect("not sent: Ref.nope.CMD is not a command", at), terminal.seen[at:])
        time.sleep(scenario.SETTLE)
        require("the refused commands put nothing on the link", len(packets(proxy, scid)) == before)

        at = len(terminal.seen)
        terminal.type("cmds NO_OP_S\r")
        require("cmds lists the dictionary", terminal.expect(r"CdhCore\.cmdDisp\.CMD_NO_OP_STRING \(0x[0-9a-f]+\) \(arg1: string\[40\]\)", at), terminal.seen[at:])
        terminal.type("quit\r")
        try:
            code = terminal.process.wait(10)
        except subprocess.TimeoutExpired:
            code = None
        require("quit leaves with exit code 0", code == 0, f"{code}")
    finally:
        terminal.read()
        with open(f"{args.out}/transcript.txt", "w") as file:
            file.write("\n".join(terminal.rows()) + "\n")

    print("without a terminal")
    stream = subprocess.Popen([args.fgds, "repl", "--url", url], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              text=True, env=ENV, start_new_session=True)
    scenario.STARTED.append(stream)
    time.sleep(2.0)
    command_send()
    time.sleep(2.0)
    stream.send_signal(signal.SIGINT)
    try:
        printed, errors = stream.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        stream.kill()
        printed, errors = stream.communicate()
    require("the stream is printed until interrupted: exit code 0", stream.returncode == 0, f"{stream.returncode} {errors}")
    require("with the command, its event and channel samples",
            f"CMD   {NO_OP}" in printed and "EVENT ACTIVITY_HI CdhCore.cmdDisp.NoOpReceived" in printed and " CHAN  " in printed, printed[-2000:])
    require("and no prompt", PROMPT not in printed)


try:
    main()
finally:
    scenario.stop_all()
if failures:
    sys.exit(f"{len(failures)} checks failed: {failures}")
print("repl: every check passed")
