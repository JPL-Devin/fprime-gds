#!/usr/bin/env python3
"""`fgds tui` on a pseudo-terminal against a stand-in for the GDS (`tests/repl/fake_gds.py`): what
stock Ref and a healthy GDS do not do.

  text of flight with escape sequences is visible text in both panes; wide characters keep the panes and the cursor in place
  colours are drawn without NO_COLOR
  one line is asked at a time: a second one is refused on the screen, which is drawn meanwhile; leaving posts nothing more
  the GDS stops and starts again with another dictionary: one line each, and that dictionary is used
  SIGTERM, SIGHUP, SIGQUIT and SIGINT put the terminal back
  terminals of a few cells are drawn and left

Standard library only.
"""
import argparse
import os
import pty
import select
import re
import signal
import subprocess
import sys
import termios
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
sys.path.insert(0, os.path.join(HERE, "..", "repl"))
import scenario  # noqa: E402
import fake_gds  # noqa: E402
from screen import Screen  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--fgds", required=True)
parser.add_argument("--out", required=True)
args = parser.parse_args()
args.fgds, args.out = os.path.abspath(args.fgds), os.path.abspath(args.out)
os.makedirs(args.out, exist_ok=True)
failures = []
ROWS, COLUMNS = 30, 120
ENV = {**os.environ, "TERM": "xterm", "NO_COLOR": "1"}
COLOUR = {name: value for name, value in ENV.items() if name != "NO_COLOR"}
HOSTILE = "a\x1b]0;TITLE\x07b\x1b[2Jc\x9b31md\r\ne\\f\u202eg"
SHOWN = "a\\x1b]0;TITLE\\x07b\\x1b[2Jc\\x9b31md\\r\\ne\\\\f\\u{202E}g"
LOST, BACK, OTHER = "trying again until the GDS answers", "the GDS answers again", "the GDS has another dictionary"


def require(what, condition, detail=""):
    print(f"  {'ok' if condition else 'FAILED'}: {what}")
    if not condition:
        failures.append(what)
        print(f"    {detail}"[:6000])


def start(fake, env=ENV, rows=ROWS, columns=COLUMNS):
    screen = Screen([args.fgds, "tui", "--url", fake.url], env, rows, columns)
    scenario.wait_for("the stream of the screen", lambda: fake.streams)
    screen.expect(lambda: "fgds>" in screen.text(), 10.0)
    return screen


def ended(screen, timeout=10.0):
    try:
        code = screen.process.wait(timeout)
    except subprocess.TimeoutExpired:
        code = None
    screen.read()
    return code


def restored(screen):
    after = termios.tcgetattr(screen.master)
    return bool(screen.modes.get(1049) is False and screen.modes.get(25) is True and after[3] & termios.ICANON and after[3] & termios.ECHO
                and after[:4] == screen.before[:4])


def start_up_error():
    print("an error before the screen")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"))
    fake.fail = {"/version": (500, HOSTILE)}
    master, slave = pty.openpty()
    process = subprocess.Popen([args.fgds, "tui", "--url", fake.url], stdin=slave, stdout=slave, stderr=slave, env=ENV, start_new_session=True)
    scenario.STARTED.append(process)
    os.close(slave)
    code, seen = process.wait(30), b""
    while select.select([master], [], [], 0.5)[0]:
        try:
            seen += os.read(master, 65536)
        except OSError:
            break
    seen = seen.decode(errors="replace")
    obeyed = [bad for bad in ("\x1b", "\x9b", "\x07", "\u202e", "d\r\ne", "d\r\r\ne") if bad in seen]
    require(f"/version answers 500: exit code 1, the message as `{SHOWN}`, no control written",
            code == 1 and f"error: 500 Internal Server Error: {SHOWN}" in seen and not obeyed, f"{code} {obeyed} {seen!r}")
    fake.stop()


def text_and_width():
    print("text of flight: escape sequences and wide characters")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"))
    screen = start(fake, COLOUR)
    try:
        fake.emit(fake_gds.event(f"A.b.{HOSTILE}", HOSTILE, "WARNING_HI"))
        fake.emit(fake_gds.channel("A.x\x1b[2J", "v\x1b]0;T\x07"))
        wide = "漢字" * 60 + "🚀end"
        fake.emit(fake_gds.event("A.b.Wide", wide))
        fake.emit(fake_gds.channel("A.b.漢字", "七 units"))
        fake.emit(fake_gds.channel("A.b.Plain", "7 units"))
        shown = screen.expect(lambda: "🚀end" in screen.events() and screen.channel("A.b.Plain"))
        require("the records are on the screen", shown, screen.text())
        require(f"the event shows the text as `{SHOWN}`", f"EVENT WARNING_HI A.b.{SHOWN}: {SHOWN}" in screen.events(), screen.text())
        require("and so does the channel table", screen.channel("A.x\\x1b[2J") == "v\\x1b]0;T\\x07", screen.text())
        bad = [bad for bad in ("\x1b]", "\x9b", "\x07", "\u202e") if bad in screen.seen]
        require("nothing of it reached the terminal as a control", not bad and "TITLE" in screen.text(), f"{bad}")
        require("the event is in red, the severity of WARNING_HI", re.search(r"\x1b\[38;5;1[;m]", screen.seen), repr(re.findall(r"\x1b\[[0-9;]*m", screen.seen)))
        require("every wide character of a long event is in the pane", screen.events().count("漢") == 60 and screen.events().count("字") == 60, screen.text())
        split = screen.split()
        borders = all(row[0] == "│" and row[split] == "│" and row[COLUMNS - 1] == "│" for row in screen.cells[1:ROWS - 4])
        require("and the borders of the panes are whole in every row", borders, screen.text())
        rows = {row.split()[0]: "".join(row).index(row.split()[1]) + sum(row.count(c) for c in "漢字") for row in screen.channels() if "units" in row}
        require("the values of the channel table are in one column, after a wide name too", rows["A.b.漢字"] == rows["A.b.Plain"], f"{rows} {screen.text()}")
        screen.type("cmd 漢字🚀x")
        screen.expect(lambda: screen.command_line() == "fgds> cmd 漢字🚀x")
        column = 1 + len("fgds> cmd ") + 6 + 1
        require(f"the cursor is after the typed wide characters, at column {column}", (screen.row, screen.column) == (ROWS - 2, column),
                f"{screen.row} {screen.column} {screen.command_line()!r}")
        screen.type("\x1b[D\x1b[D")  # Left, twice: before the rocket
        screen.expect(lambda: screen.column == column - 3)
        require("and two columns back for a wide character", screen.column == column - 3, f"{screen.column}")
        with open(f"{args.out}/wide.txt", "w") as file:
            file.write(screen.text() + "\n")
        screen.type("\x03quit\r")
        require("quit leaves with exit code 0 and puts the terminal back", ended(screen) == 0 and restored(screen), f"{screen.modes}")
    finally:
        fake.stop()


def one_at_a_time():
    print("one line is asked at a time")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.ONE", "A.b.TWO", "A.b.THREE"))
    fake.delay = 4.0
    screen = start(fake)
    try:
        screen.type("cmd A.b.ONE\rcmd A.b.TWO\rcmd A.b.THREE\rmute channels\r")
        refusal = "not sent: the GDS has not answered `cmd A.b.ONE`; type the line again after its answer"
        refused = screen.expect(lambda: screen.events().count(refusal) == 2)
        require("while the first is not answered, the next two are refused on the screen", refused and len(fake.posts) == 1, f"{fake.posts} {screen.text()}")
        require("a line that asks the GDS nothing is answered meanwhile", "muted: channels" in screen.events(), screen.text())
        require("the command pane says what is waited for", "waiting for the GDS to answer `cmd A.b.ONE`" in screen.text(), screen.text())
        screen.type("typed meanwhile")
        drawn = screen.expect(lambda: screen.command_line() == "fgds> typed meanwhile", 2.0)
        require("and the screen is drawn while waiting", drawn and len(fake.posts) == 1 and "ONE [] by" not in screen.events(), screen.text())
        answered = screen.expect(lambda: "CMD   A.b.ONE [] by" in screen.events())
        require("the answer of the first is shown", answered and "waiting for the GDS" not in screen.text(), screen.text())
        time.sleep(1.0)
        require("the refused lines were not kept: one request reached the GDS", [body["name"] for _, body in fake.posts] == ["A.b.ONE"], f"{fake.posts}")
        screen.type("\x15cmd A.b.TWO\r")
        scenario.wait_for("the second request", lambda: len(fake.posts) == 2)
        screen.type("cmd A.b.THREE\r\x04")  # refused, then Ctrl-D on the empty line
        began = time.monotonic()
        code = ended(screen)
        require("leaving does not wait for the answer", code == 0 and time.monotonic() - began < 2.0, f"{code} {time.monotonic() - began}")
        require("puts the terminal back", restored(screen), f"{screen.modes}")
        said = "fgds tui: left before the GDS answered `cmd A.b.TWO`: it was posted and may have been sent"
        require("and says which line was not answered", said in screen.seen.replace("\r\n", "\n"), repr(screen.seen[-600:]))
        time.sleep(fake.delay + 1.0)
        require("nothing is posted after leaving", [body["name"] for _, body in fake.posts] == ["A.b.ONE", "A.b.TWO"], f"{fake.posts}")
    finally:
        fake.stop()


def restart():
    print("the GDS stops and starts again with another dictionary")
    port = scenario.free_port()
    fake = fake_gds.Fake(port, fake_gds.dictionary("A.b.GO"))
    screen = start(fake)
    try:
        fake.stop()
        require("the GDS stops: the events pane says so", screen.expect(lambda: LOST in screen.events()), screen.text())
        screen.type("cmd A.b.GO\r")
        require("a command says it was not sent", screen.expect(lambda: "not sent: no GDS answers at" in screen.events()), screen.text())
        time.sleep(8.0)
        screen.read()
        require("eight seconds without a GDS add no line", screen.events().count(LOST) == 1 and BACK not in screen.events(), screen.text())
        fake = fake_gds.Fake(port, fake_gds.dictionary("A.b.GO", "Other.x.NEW"))  # the same version string
        back = screen.expect(lambda: BACK in screen.events() and OTHER in screen.events(), 40.0)
        require("the GDS starts again with another dictionary: one line for each",
                back and screen.events().count(BACK) == 1 and screen.events().count(OTHER) == 1 and screen.events().count(LOST) == 1, screen.text())
        fake.emit(fake_gds.channel("A.b.Count", "7"))
        require("its channel samples fill the table", screen.expect(lambda: screen.channel("A.b.Count") == "7"), screen.text())
        screen.type("cmd NE\t")
        require("Tab completes from the new dictionary", screen.expect(lambda: screen.command_line() == "fgds> cmd Other.x.NEW"), screen.text())
        screen.type("\r")
        sent = screen.expect(lambda: "CMD   Other.x.NEW [] by" in screen.events())
        require("and a command reaches the new GDS", sent and fake.posts == [("/commands", {"name": "Other.x.NEW", "args": []})], f"{fake.posts} {screen.text()}")
        with open(f"{args.out}/restart.txt", "w") as file:
            file.write(screen.text() + "\n")
        screen.type("quit\r")
        require("quit leaves with exit code 0", ended(screen) == 0)
    finally:
        fake.stop()


def signals_and_sizes():
    print("signals")
    fake = fake_gds.Fake(scenario.free_port(), fake_gds.dictionary("A.b.GO"))
    try:
        for number in (signal.SIGTERM, signal.SIGHUP, signal.SIGQUIT, signal.SIGINT, signal.SIGALRM, signal.SIGUSR1, signal.SIGUSR2):
            screen = start(fake)
            screen.type("half a li")
            screen.expect(lambda: screen.command_line() == "fgds> half a li")
            before = termios.tcgetattr(screen.master)
            require(f"{number.name}: the screen is the alternate one and the terminal is raw", screen.modes.get(1049) and not before[3] & termios.ICANON)
            screen.process.send_signal(number)
            code = ended(screen)
            require(f"{number.name}: fgds tui ends with exit code {128 + number}", code == 128 + number, f"{code}")
            require(f"{number.name}: on the main screen, cursor shown, line mode with echo, as it was", restored(screen), f"{screen.modes}")

        print("terminals of a few cells")
        for rows, columns in ((1, 1), (2, 5), (4, 12), (8, 30)):
            screen = start(fake, rows=rows, columns=columns)
            fake.emit(fake_gds.event("A.b.Wide", "漢字" * 20))
            fake.emit(fake_gds.channel("A.b.Count", "7"))
            screen.type("cmd GO\t\thelp 漢字漢字漢字漢字漢字漢字漢字\r\x1b[5~\x1b[6~\x1b[1;3A")
            time.sleep(1.0)
            screen.read()
            alive = screen.process.poll() is None
            screen.type("\x03quit\r")
            code = ended(screen)
            require(f"{rows} by {columns}: drawn, typed into, and left with exit code 0 and the terminal put back", alive and code == 0 and restored(screen),
                    f"{alive} {code} {screen.seen[-2000:]!r}")
    finally:
        fake.stop()


try:
    start_up_error()
    text_and_width()
    one_at_a_time()
    restart()
    signals_and_sizes()
finally:
    scenario.stop_all()
if failures:
    sys.exit(f"{len(failures)} checks failed: {failures}")
print("tui faults: every check passed")
