"""A process on a pseudo-terminal, for the checks of `fgds repl`. Standard library only."""
import os
import pty
import re
import select
import subprocess
import sys
import termios
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402


class Terminal:
    """A process on a pseudo-terminal, and everything it wrote there."""

    def __init__(self, command, env):
        self.master, slave = pty.openpty()
        self.before = termios.tcgetattr(slave)
        self.process = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave, env=env, start_new_session=True)
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

    def ended(self, timeout=10.0):
        """The exit code of the process; None when it is still there after `timeout` seconds."""
        try:
            return self.process.wait(timeout)
        except subprocess.TimeoutExpired:
            return None

    def line_mode(self):
        """Whether the terminal is as it was before the process: line mode, echo, the same flags."""
        after = termios.tcgetattr(self.master)
        return bool(after[3] & termios.ICANON and after[3] & termios.ECHO and after[:4] == self.before[:4])
