"""The screen a program draws on a pseudo-terminal, for the checks of `fgds tui`. Standard library only."""
import codecs
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time
import unicodedata

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402

ROWS, COLUMNS = 45, 180  # of the one screen a check has at a time


class Screen:
    """A process on a pseudo-terminal of `rows` by `columns`, and the screen it has drawn there.

    Knows what the program writes: text, cursor moves, erasing, and the switch to the alternate
    screen and back. Colours and other modes are read and dropped.
    """

    def __init__(self, command, env, rows=45, columns=180):
        global ROWS, COLUMNS
        ROWS, COLUMNS = rows, columns
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
        self.before = termios.tcgetattr(slave)
        self.process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
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
                # A wide character (CJK, emoji) takes two cells: the second holds nothing.
                for cell in (char, "") if unicodedata.east_asian_width(char) in "WF" else (char,):
                    if self.column < COLUMNS:
                        self.cells[self.row][self.column] = cell
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
