#!/usr/bin/env python3
"""`fgds repl` on a pseudo-terminal against a stand-in for the GDS (`fake_gds.py`): what stock Ref
and a healthy GDS do not do.

  text of flight with escape sequences reaches the terminal as visible text, with colour and without, and when piped
  a GDS without one of the record kinds, and one whose first sequence number is 0
  a command the dictionary read at the start does not have is left to the GDS
  the GDS stops: one line, then silence; it starts again with another dictionary under the same version: one line, and that dictionary is used
  an error before the prompt, and the line being typed or pasted, show no control either
  a GDS that answers and refuses the stream: one line
  SIGTERM, SIGHUP, SIGQUIT, SIGINT, SIGALRM, SIGUSR1 and SIGUSR2 put the terminal back and keep the history

Standard library only.
"""
import argparse
import os
import re
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
import scenario  # noqa: E402
import fake_gds  # noqa: E402
from terminal import Terminal  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--fgds", required=True)
parser.add_argument("--out", required=True)
args = parser.parse_args()
args.fgds, args.out = os.path.abspath(args.fgds), os.path.abspath(args.out)
os.makedirs(args.out, exist_ok=True)
failures = []
PROMPT = "fgds> "
ENV = {**os.environ, "HOME": args.out, "TERM": "xterm", "NO_COLOR": "1"}
COLOUR = {name: value for name, value in ENV.items() if name != "NO_COLOR"}
# A window title, a cleared screen, a C1 control, a line break, a backslash, a reversal of direction.
# and one of each other class of the rule: NUL, backspace, DEL, a C1 line break, a tab, a mark without width, an isolate, a BOM.
HOSTILE = "a\x1b]0;TITLE\x07b\x1b[2Jc\x9b31md\r\ne\\f\u202eg\x00\x08\x7f\x85\t\u200b\u2066\ufeffh"
SHOWN = "a\\x1b]0;TITLE\\x07b\\x1b[2Jc\\x9b31md\\r\\ne\\\\f\\u{202E}g\\x00\\x08\\x7f\\x85\\t\\u{200B}\\u{2066}\\u{FEFF}h"
LOST, BACK, OTHER = "trying again until the GDS answers", "the GDS answers again", "the GDS has another dictionary"


def require(what, condition, detail=""):
    print(f"  {'ok' if condition else 'FAILED'}: {what}")
    if not condition:
        failures.append(what)
        print(f"    {detail}"[:3000])


def obeyed(seen):
    """What of HOSTILE a terminal would obey, in what was written to it."""
    return [bad for bad in ("\x1b]", "\x1b[2J", "\x9b", "\x07", "\u202e", "d\r\ne", "d\r\r\ne", "\x00", "\x08", "\x7f", "\x85", "\t", "\u200b", "\u2066", "\ufeff") if bad in seen]


def prompt(fake, env=ENV):
    terminal = Terminal([args.fgds, "repl", "--url", fake.url], env)
    require("the prompt is drawn", terminal.expect(re.escape(PROMPT)), terminal.seen)
    scenario.wait_for("the stream of the prompt", lambda: fake.streams)
    return terminal


def hostile_text():
    print("text of flight with escape sequences")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO", f"A.b.{HOSTILE}"))
    for name, env in (("without colour", ENV), ("with colour", COLOUR)):
        terminal = prompt(fake, env)
        at = len(terminal.seen)
        fake.emit(fake_gds.event(f"A.b.{HOSTILE}", HOSTILE, "WARNING_HI"))
        fake.emit(fake_gds.channel(f"A.b.{HOSTILE}", HOSTILE))
        fake.emit({"kind": "fprime.command", "raw": "", "body": {"name": HOSTILE, "args": [HOSTILE], "client": HOSTILE,
                                                             "outcome": "refused", "reason": HOSTILE, "opcode": 1}})
        fake.emit(fake_gds.event("A.b.Last", "the last one"))
        require(f"{name}: the records are printed", terminal.expect("the last one", at), terminal.seen[at:])
        seen = terminal.seen[at:]
        require(f"{name}: event, channel and command show the text as `{SHOWN}`",
                f"EVENT WARNING_HI A.b.{SHOWN}: {SHOWN}" in re.sub(r"\x1b\[\d+m", "", seen) and f"CHAN  A.b.{SHOWN} = {SHOWN}" in seen
                and f"CMD   {SHOWN} " in seen, repr(seen))
        at = len(terminal.seen)
        terminal.type("cmds\r")
        require(f"{name}: the dictionary lists a name as visible text", terminal.expect(re.escape(f"A.b.{SHOWN} (0x"), at), repr(terminal.seen[at:]))
        fake.refuse = HOSTILE
        terminal.type("cmd A.b.GO\r")
        require(f"{name}: the reason of a refusal is visible text", terminal.expect(re.escape(f"not sent: {SHOWN}"), at), repr(terminal.seen[at:]))
        fake.refuse = None
        terminal.type("quit\r")
        require(f"{name}: quit leaves with exit code 0", terminal.ended() == 0)
        terminal.read()
        require(f"{name}: nothing of it reached the terminal as a control", not obeyed(terminal.seen), f"{obeyed(terminal.seen)} {terminal.seen!r}")
        coloured = "\x1b[31mWARNING_HI\x1b[0m" in terminal.seen
        require(f"{name}: the severity is {'in red' if env is COLOUR else 'plain'}", coloured == (env is COLOUR), repr(terminal.seen))

    piped = subprocess.Popen([args.fgds, "repl", "--url", fake.url], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             env=ENV, start_new_session=True, preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
    scenario.STARTED.append(piped)
    scenario.wait_for("the stream of the piped prompt", lambda: fake.streams)
    fake.emit(fake_gds.event(f"A.b.{HOSTILE}", HOSTILE))
    fake.emit(fake_gds.channel(f"A.b.{HOSTILE}", HOSTILE))
    time.sleep(1.0)
    piped.send_signal(signal.SIGINT)
    printed = piped.communicate(timeout=10)[0].decode()
    require("piped: the same visible text, one record a line",
            f"EVENT ACTIVITY_HI A.b.{SHOWN}: {SHOWN}\n" in printed and f"CHAN  A.b.{SHOWN} = {SHOWN}\n" in printed and printed.count("\n") == 2
            and not obeyed(printed), repr(printed))
    fake.stop()


def start_up_errors():
    print("an error before the prompt")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"))
    for path in ("/version", "/dictionary"):
        fake.fail = {path: (500, HOSTILE)}
        terminal = Terminal([args.fgds, "repl", "--url", fake.url], ENV)
        code = terminal.ended()
        terminal.read()
        require(f"{path} answers 500: the prompt ends with exit code 1 and the message as `{SHOWN}`",
                code == 1 and f"error: 500 Internal Server Error: {SHOWN}" in terminal.seen and not obeyed(terminal.seen), f"{code} {terminal.seen!r}")
    piped = subprocess.run([args.fgds, "repl", "--url", fake.url], stdin=subprocess.DEVNULL, capture_output=True, env=ENV, timeout=30)
    printed = piped.stderr.decode(errors="replace")
    require("piped: the same", piped.returncode == 1 and f"error: 500 Internal Server Error: {SHOWN}" in printed and not obeyed(printed), repr(printed))
    fake.stop()
    terminal = Terminal([args.fgds, "repl", "--url", f"http://127.0.0.1:1/{HOSTILE}".replace("\x00", "")], ENV)  # an argument cannot hold NUL
    code = terminal.ended()
    terminal.read()
    require("no GDS at `--url`: the address is in the error as visible text",
            code == 1 and "error: no GDS answers at http://127.0.0.1:1/a\\x1b]0;TITLE" in terminal.seen and not obeyed(terminal.seen), f"{code} {terminal.seen!r}")


def typed_text():
    print("what is typed or pasted")
    history = f"{args.out}/.fgds_history"
    with open(history, "w") as file:  # as `c381880f` could leave it
        file.write("#V2\ncmds plain\ncmds \x07\u202e\n")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"), first_seq=0)
    terminal = prompt(fake)
    at = len(terminal.seen)
    terminal.type("cmd A.b.GO\r")
    answered = terminal.expect(r"CMD   A\.b\.GO \[\] by fgds-repl-\d+: sent", at)
    require("a command whose record has sequence number 0 is answered", answered and [r["seq"] for r in fake.records] == ["0"],
            f"{fake.records} {terminal.seen[at:]!r}")
    at = len(terminal.seen)
    # Pasted: U+202E, a C1 control and marks without width between letters; then a quoted insert of BEL.
    terminal.type("cmd A.b.GO x\u202e\u009b\u200b\ufeffZ")
    terminal.type(b"\x16\x07\x11\x07")
    terminal.type("\r")
    sent = terminal.expect(r"CMD   A\.b\.GO \[\"xZ\"\] by fgds-repl-\d+: sent", at)
    require("characters that would not be shown as they are do not enter the line: the argument sent is what the row shows",
            sent and fake.posts[-1] == ("/commands", {"name": "A.b.GO", "args": ["xZ"]}) and "cmd A.b.GO xZ" in terminal.rows()[-5:-1].__str__(),
            f"{fake.posts} {terminal.seen[at:]!r}")
    at = len(terminal.seen)
    terminal.type(b"cmds \x9b")  # one byte that is not UTF-8
    dropped = terminal.expect("line dropped: ", at)
    terminal.type("cmds GO\r")
    require("a byte that is not text drops the line, and the prompt goes on", dropped and terminal.expect(r"A\.b\.GO \(0x", at), repr(terminal.seen[at:]))
    at = len(terminal.seen)
    terminal.type("\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A")
    time.sleep(0.5)
    terminal.type("\x15quit\r")
    require("quit leaves with exit code 0", terminal.ended() == 0)
    terminal.read()
    require("the lines of the history were recalled, the one with controls was not", "cmds plain" in terminal.seen[at:], repr(terminal.seen[at:]))
    require("nothing reached the terminal as a control", not obeyed(terminal.seen), f"{obeyed(terminal.seen)} {terminal.seen!r}")
    kept = open(history).read()
    require("and the history holds the line as sent", "cmd A.b.GO xZ\n" in kept and not obeyed(kept.replace("\x07\u202e", "", 1)), repr(kept))
    fake.stop()


def stream_refused():
    print("a GDS that answers and has no stream")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"))
    fake.no_stream = True
    terminal = Terminal([args.fgds, "repl", "--url", fake.url], ENV)
    lost = terminal.expect(LOST)
    time.sleep(8.0)
    terminal.read()
    require("one line says so, and eight seconds add none", lost and terminal.seen.count(LOST) == 1 and "404" in terminal.seen, terminal.seen)
    at = len(terminal.seen)
    terminal.type("cmd A.b.GO\r")
    require("a command still goes through", terminal.expect(r"CMD   A\.b\.GO \[\] by fgds-repl-\d+: sent", at), terminal.seen[at:])
    fake.no_stream = False
    require("the stream is there again: one line", terminal.expect(BACK, at, 40.0) and terminal.seen.count(BACK) == 1, terminal.seen[at:])
    terminal.type("quit\r")
    require("quit leaves with exit code 0", terminal.ended() == 0)
    fake.stop()


def judge_and_restart():
    print("a GDS that is not the one the prompt started with")
    port = scenario.free_port()
    fake = fake_gds.Fake(port, fake_gds.dictionary("A.b.GO"), kinds=("fprime.event", "fprime.command"))
    terminal = prompt(fake)
    try:
        missing = terminal.expect("the GDS has no records of kind fprime.channel: none are shown")
        fake.emit(fake_gds.event("A.b.Seen", "an event"))
        require("without the channel records, one line says so and events are printed", missing and terminal.expect("A.b.Seen: an event"), terminal.seen)

        at = len(terminal.seen)
        terminal.type("cmd Other.x.NEW 1\r")
        sent = terminal.expect(r"CMD   Other\.x\.NEW \[\"1\"\] by fgds-repl-\d+: sent", at)
        require("a command the dictionary read here does not have is left to the GDS, which accepts it",
                sent and fake.posts == [("/commands", {"name": "Other.x.NEW", "args": ["1"]})], f"{fake.posts} {terminal.seen[at:]}")
        fake.refuse = "unknown command Other.x.NEW"
        terminal.type("cmd Other.x.NEW\r")
        require("and its refusal is the reason of the GDS", terminal.expect("not sent: unknown command Other.x.NEW", at), terminal.seen[at:])

        at = len(terminal.seen)
        fake.stop()
        lost = terminal.expect(LOST, at)
        terminal.type("cmd A.b.GO\r")
        refused = terminal.expect(r"not sent: no GDS answers at [^\r\n]*", at)
        require("the GDS stops: one line says so, and a command says it was not sent", lost and refused, terminal.seen[at:])
        time.sleep(8.0)
        terminal.read()
        notes = [line for line in terminal.seen[at:].split("\n") if LOST in line or BACK in line]
        require("eight seconds without a GDS add no line", len(notes) == 1, f"{notes}")

        at = len(terminal.seen)
        fake = fake_gds.Fake(port, fake_gds.dictionary("A.b.GO", "Other.x.NEW"))  # the same version string
        back = terminal.expect(BACK, at, 40.0)
        require("the GDS starts again: one line says so", back and terminal.seen[at:].count(BACK) == 1 and LOST not in terminal.seen[at:], terminal.seen[at:])
        require("and one that it has another dictionary", terminal.expect(OTHER, at) and terminal.seen[at:].count(OTHER) == 1, terminal.seen[at:])
        fake.emit(fake_gds.event("A.b.Again", "after the restart"))
        require("its events are printed", terminal.expect("A.b.Again: after the restart", at), terminal.seen[at:])
        at = len(terminal.seen)
        terminal.type("cmds NEW\r")
        require("cmds lists the new dictionary", terminal.expect(r"Other\.x\.NEW \(0x", at), terminal.seen[at:])
        at = len(terminal.seen)
        terminal.type("cmd NE\t")
        require("Tab completes from it", terminal.expect(r"cmd Other\.x\.NEW", at), terminal.seen[at:])
        terminal.type("\r")
        require("and a command reaches the new GDS", terminal.expect(r"CMD   Other\.x\.NEW \[\] by fgds-repl-\d+: sent", at)
                and fake.posts == [("/commands", {"name": "Other.x.NEW", "args": []})], f"{fake.posts} {terminal.seen[at:]}")
        terminal.type("quit\r")
        require("quit leaves with exit code 0", terminal.ended() == 0)
    finally:
        terminal.read()
        with open(f"{args.out}/restart.txt", "w") as file:
            file.write("\n".join(terminal.rows()) + "\n")
        fake.stop()


def signals():
    print("signals")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"))
    history = f"{args.out}/.fgds_history"
    for number in (signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT, signal.SIGINT, signal.SIGALRM, signal.SIGUSR1, signal.SIGUSR2):
        if os.path.exists(history):
            os.remove(history)
        terminal = prompt(fake)
        at = len(terminal.seen)
        terminal.type(f"cmds {number.name}\r")
        time.sleep(0.5)  # the line is entered
        terminal.type("half a li")
        terminal.expect("half a li", at)
        require(f"{number.name}: the terminal is raw while the prompt reads", not terminal.line_mode())
        terminal.process.send_signal(number)
        code = terminal.ended()
        require(f"{number.name}: the prompt ends with exit code {128 + number}", code == 128 + number, f"{code}")
        require(f"{number.name}: the terminal is in line mode with echo, as it was", terminal.line_mode())
        terminal.read()
        require(f"{number.name}: bracketed paste is not left on", not terminal.seen.rstrip().endswith("\x1b[?2004h") and "\x1b[?2004h" not in terminal.seen,
                repr(terminal.seen[-80:]))
        kept = os.path.exists(history) and open(history).read()
        require(f"{number.name}: the history has the line entered before", kept and f"cmds {number.name}" in kept, f"{kept!r}")
    fake.stop()


try:
    hostile_text()
    start_up_errors()
    typed_text()
    stream_refused()
    judge_and_restart()
    signals()
finally:
    scenario.stop_all()
if failures:
    sys.exit(f"{len(failures)} checks failed: {failures}")
print("repl faults: every check passed")
