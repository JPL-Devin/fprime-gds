"""Lines the `Command String` input refuses, then lines it takes, each followed by the send button.

    refused.py record PYTHON_GDS_URL PORT FIXTURE    types the sequences in a running Python GDS page; PORT is where
                                                     its link waits for Ref, and the frames it writes there are kept
    refused.py check FGDS DICTIONARY FIXTURE OUT     types them in the new page

Each line is typed over the one before (select all, delete, type, Enter). A line that is a command with all its
arguments (True), and a line that leaves arguments out but names the command the form shows (HELD), must do as
the Python GDS page: send the command packet inside the frame that page sent, or nothing when it sent nothing.
After any other line (False) the Python page sends the command its form still holds (deviations.md, R6-3) and
the new page must send nothing. CLEAR is the `Clear Arguments` button, not a line. A line with CLICK is typed
with no Enter: the click on the send button is what makes the page read it.
For a HELD line that gives a value the input of the new page shows the line of its form (a name alone stays); the Python page shows it only when a value
changed (deviations.md), so that text is compared with the values of the form, not with the Python page.
"""
import base64
import json
import os
import socket
import sys
import time

from playwright.sync_api import sync_playwright

import harness
import line
import scenario

NO_OP, TEST, STRING = "CdhCore.cmdDisp.CMD_NO_OP", "CdhCore.cmdDisp.CMD_TEST_CMD_1, 5, 1.5, 7", 'CdhCore.cmdDisp.CMD_NO_OP_STRING, "a"'
ONE = "CdhCore.cmdDisp.CMD_TEST_CMD_1"
VERSION, REMOVE = "CdhCore.version.VERSION", 'FileHandling.fileManager.RemoveFile, "x7.bin"'
HELD, CLEAR, CLICK, ALIVE = "held", "<Clear Arguments>", "click", b"sitting well".hex()
# (line, True: a command with all its arguments; HELD: part of the arguments of the command shown; False: any other)
SEQUENCES = [
    [(NO_OP, True), (",", False), (NO_OP, True), (NO_OP, True), (NO_OP + " ", True)],
    [(TEST, True), (",", False), (TEST, True), (ONE + ", 5", HELD), (TEST, True), (", ,", False), (TEST, True)],
    [(TEST, True), (",", False), (ONE + ", 5", HELD), (TEST, True)],
    [(STRING, True), (";", False), (STRING, True), (NO_OP, True), ('CdhCore.cmdDisp.CMD_NO_OP_STRING, "unfinished', False), (NO_OP, True)],
    [(TEST, True), ("Nope.cmd.DOES_NOT_EXIST", False), (TEST, True), ("CdhCore.cmdDisp.CMD_TEST_CMD_1, 5, 1e3, 7", False), (TEST, True),
     (ONE + ", 6", HELD), (ONE + ", 6, 1.5, 7", True)],
    # Part of the arguments of the command shown: the form sends what it holds, or marks what it cannot send.
    [(TEST, True), (ONE + ", 6", HELD), (ONE + ", 8, 2.5", HELD), (ONE, HELD), (ONE + ", 9, 1e3", False), (ONE + ", , 3.5", False),
     (ONE + ", 300", HELD), (ONE + ", 4", HELD)],
    [(TEST, True), (CLEAR, None), (ONE + ", 5", HELD), (ONE + ", 5, 1.5", HELD), (ONE + ", 5, 1.5, 7", True)],
    [(TEST, True), (NO_OP, True), (ONE + ", 6", False), (TEST, True), (STRING, True), (ONE, False), ("CdhCore.cmdDisp.CMD_NO_OP_STRING", HELD)],
    # The click on the send button with no Enter before.
    [(TEST, True), (ONE + ", 6", HELD, CLICK), (ONE + ", 5", HELD, CLICK), (ONE + ", 5", HELD, CLICK), (TEST, True, CLICK),
     (ONE + ", 8, 2.5, 9", True, CLICK), (",", False, CLICK), (ONE, HELD, CLICK), (NO_OP, True, CLICK), (ONE + ", 6", False, CLICK)],
    [('Ref.typeDemo.CHOICES, [ONE, TWO]', True), ('Ref.typeDemo.CHOICES, [RED]', HELD), ('Ref.typeDemo.CHOICES', HELD),
     ('FileHandling.fileManager.RemoveFile, "a.bin", false', True), ('FileHandling.fileManager.RemoveFile, "b.bin"', HELD)],
    # An enum constant in another case, part of one, with its type, as a number (round 7, finding 47).
    [(VERSION + ", all", True), (VERSION + ", Project", True), (VERSION + ", aLL", True), (VERSION + ", AL", True), (VERSION + ", 4", True),
     (VERSION + ", VersionType.ALL", True), (VERSION + ", Svc.VersionType.ALL", True), (VERSION + ", CUSTOM", True), (VERSION, HELD),
     ("Ref.typeDemo.CHOICE, red", True), ("Ref.typeDemo.CHOICES, [one, Two]", True), ("Ref.typeDemo.CHOICE, 2", True)],
    # The texts of a boolean (finding 49).
    [(REMOVE + ", 1", True), (REMOVE + ", TRUE", True), (REMOVE + ", yes", True), (REMOVE + ", No", True), (REMOVE + ", FALSE", True),
     (REMOVE + ", 0", True), (REMOVE + ", true", True), (REMOVE + ", on", True), (REMOVE + ", False", True), (REMOVE, HELD)],
    # A name alone, of the command shown and of another (finding 49).
    [(STRING, True), ("CdhCore.cmdDisp.CMD_NO_OP_STRING", HELD), (NO_OP, True), ("CdhCore.cmdDisp.CMD_NO_OP_STRING", False),
     (TEST, True), (NO_OP, True), (ONE, False), (NO_OP, True)],
    # The click on the send button with no Enter, after a whole line of another command than the one shown: an
    # enum, a boolean, a string, numbers, an array, no argument (round 8, finding 51). The same command again sends.
    [(STRING, True), (VERSION + ", all", True, CLICK), (VERSION + ", ALL", True, CLICK), ("Ref.typeDemo.CHOICE, ONE", True, CLICK),
     (REMOVE + ", true", True, CLICK), (STRING, True, CLICK), (TEST, True, CLICK), ("Ref.typeDemo.CHOICES, [ONE, TWO]", True, CLICK),
     (TEST, True, CLICK), (NO_OP, True, CLICK), (VERSION + ", project", True, CLICK), ("Ref.typeDemo.CHOICE, ONE", True, CLICK),
     (STRING, True, CLICK), (REMOVE + ", true", True, CLICK)],
    # An enum constant in another case that changes no value of the form shown (finding 52).
    [(VERSION + ", PROJECT", True), (VERSION + ", project", True), (VERSION + ", Project", True, CLICK)],
]


def enter(page, selector, text, key=True):
    page.click(selector)
    page.keyboard.press("Control+a")
    page.keyboard.press("Delete")
    page.keyboard.type(text)
    if key:
        page.keyboard.press("Enter")
        page.wait_for_timeout(300)


def record(url, port, fixture):
    link = socket.create_connection(("127.0.0.1", port))
    link.settimeout(0.2)

    def frames():
        """What the link wrote in the last second and a half (its keep-alive packets too)."""
        data, end = b"", time.monotonic() + 1.5
        while time.monotonic() < end:
            try:
                data += link.recv(65536)
            except socket.timeout:
                pass
        return data.hex()

    cases = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 2000})
        for sequence in SEQUENCES:
            page.goto(url)
            page.wait_for_selector(".command-input-form")
            frames()
            case = []
            for typed, whole, *click in sequence:
                if typed == CLEAR:
                    page.click("button:has-text('Clear Arguments')")
                    case.append({"typed": typed})
                    continue
                enter(page, "input[name=command-text]", typed, not click)
                if not click:
                    shown = page.evaluate(line.PYTHON_SHOWN)
                page.click("button:has-text('Send Command')")
                if click:
                    page.wait_for_timeout(300)
                    shown = page.evaluate(line.PYTHON_SHOWN)
                case.append({"typed": typed, "whole": whole, "click": bool(click), "line": shown["line"], "name": shown["name"],
                             "inputs": shown["inputs"], "frame": frames()})
            cases.append(case)
        browser.close()
    with open(fixture, "w") as file:
        json.dump(cases, file, indent=1)
        file.write("\n")
    print(f"{len(cases)} sequences written to {fixture}")


def check(binary, dictionary, fixture, out):
    os.makedirs(out, exist_ok=True)
    with open(fixture) as file:
        cases = json.load(file)
    gds = harness.Gds(binary, dictionary, out)

    def last():
        return gds.get("/records?kind=fprime.command&limit=10000")["records"][-1:]

    different, sent, held, nothing, switched, moved = [], 0, 0, 0, 0, 0
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            page = harness.open_page(browser, gds.url, [])
            for case in cases:
                page.reload()
                page.wait_for_selector("body[data-ready=true]")
                harness.show(page, "commands")
                for step in case:
                    if step["typed"] == CLEAR:
                        page.click("#command-clear")
                        continue
                    where = ([s["typed"] for s in case], step["typed"])
                    click, before = step.get("click"), last()
                    # A click that makes the page read a line of another command than the one shown.
                    other = click and step["whole"] is True and page.evaluate(line.NEW_SHOWN)["name"] != step["name"]
                    enter(page, "#command-string", step["typed"], not click)
                    if click:
                        page.click("#command-send")
                        page.wait_for_timeout(300)
                    shown = page.evaluate(line.NEW_SHOWN)
                    shown["inputs"], wanted = line.plain(shown["inputs"]), {**step, "inputs": line.plain(step["inputs"])}
                    if step["whole"] == HELD:
                        full = ", ".join([step["name"], *shown["inputs"]])
                        if shown["name"] != step["name"] or shown["inputs"] != wanted["inputs"]:
                            different.append((*where, f"form: Python GDS page {wanted['inputs']}, new page {shown['inputs']}"))
                        given = "," in step["typed"]
                        if (shown["line"] == step["typed"]) == given or (
                                given and all(shown["inputs"]) and "[" not in shown["line"] and shown["line"].replace('"', "") != full):
                            different.append((*where, f"the input shows {shown['line']!r}, the form {full!r}"))
                    # With another command shown, the form of the line's command is not on screen.
                    elif step["whole"] or shown["name"] == step["name"]:
                        for key in ("line", "name") + (("inputs",) if step["whole"] else ()):
                            if shown[key] != wanted[key]:
                                different.append((*where, f"{key}: Python GDS page {wanted[key]!r}, new page {shown[key]!r}"))
                    if not click:
                        page.click("#command-send")
                    try:
                        scenario.wait_for("the command", lambda: last() != before, 2 if step["whole"] else 1)
                        raw = base64.b64decode(last()[0]["raw"]).hex()
                    except TimeoutError:
                        raw = None
                    if other:
                        # The Python page sends when its send button stays under the pointer as the form of the
                        # other command is drawn; the new page never sends (deviations.md).
                        switched += 1
                        moved += bool(step["frame"].replace(ALIVE, ""))
                        if raw is not None:
                            different.append((*where, f"a click that changes the command shown sent {raw}"))
                    elif step["whole"] and not step["frame"].replace(ALIVE, ""):
                        nothing += 1
                        if raw is not None:
                            different.append((*where, f"the Python GDS page sent nothing, the new page {raw}"))
                    elif step["whole"]:
                        sent += 1
                        held += step["whole"] == HELD
                        if raw is None or raw not in step["frame"]:
                            different.append((*where, f"Python GDS page sent {step['frame']}, new page {raw}"))
                    elif raw is not None:
                        different.append((*where, f"a line that is not a command, the new page sent {raw}"))
            # A line that leaves arguments out and names a command that is not shown: nothing sent, its form untouched.
            enter(page, "#command-string", TEST)
            enter(page, "#command-string", NO_OP)
            enter(page, "#command-string", ONE + ", 6")
            before = last()
            page.click("#command-send")
            page.wait_for_timeout(1000)
            page.select_option("#command-select", ONE)
            page.wait_for_timeout(300)
            hidden = page.evaluate(line.NEW_SHOWN)["inputs"]
            if last() != before or hidden != ["5", "1.5", "7"]:
                different.append(([TEST, NO_OP], ONE + ", 6", f"sent {last() != before}, the form of the command then holds {hidden}"))
            browser.close()
    finally:
        gds.stop()
    for typed, at, text in different:
        print(f"DIFFERENT {typed} at {at!r}: {text}")
    if different:
        sys.exit(f"{len(different)} differences from the Python GDS page")
    print(f"  ok: {len(cases)} sequences of refused and taken Command String lines, each then the send button: {sent} "
          f"lines send the command packet of the Python GDS page's frame ({held} of them give part of the arguments of the "
          f"command shown), {nothing} such lines send nothing on either page, the lines not taken send nothing; "
          f"{switched} clicks that make the page read a whole line of another command send nothing ({moved} of them send "
          "on the Python GDS page, whose send button stayed under the pointer); "
          "such a line for a command that is not shown leaves the form of that command as it was")


if __name__ == "__main__":
    if sys.argv[1] == "record":
        record(sys.argv[2], int(sys.argv[3]), sys.argv[4])
    else:
        check(*sys.argv[2:6])
