"""Lines the `Command String` input refuses, then lines it takes, each followed by the send button.

    refused.py record PYTHON_GDS_URL PORT FIXTURE    types the sequences in a running Python GDS page; PORT is where
                                                     its link waits for Ref, and the frames it writes there are kept
    refused.py check FGDS DICTIONARY FIXTURE OUT     types them in the new page

Each line is typed over the one before (select all, delete, type, Enter). A line that is a command with all its
arguments must send the command packet inside the frame the Python GDS page sent; after any other line the Python
page sends the command its form still holds (deviations.md, R6-3) and the new page must send nothing.
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
# (line, whether it is a command with all its arguments)
SEQUENCES = [
    [(NO_OP, True), (",", False), (NO_OP, True), (NO_OP, True), (NO_OP + " ", True)],
    [(TEST, True), (",", False), (TEST, True), ("CdhCore.cmdDisp.CMD_TEST_CMD_1, 5", False), (TEST, True), (", ,", False), (TEST, True)],
    [(TEST, True), (",", False), ("CdhCore.cmdDisp.CMD_TEST_CMD_1, 5", False), (TEST, True)],
    [(STRING, True), (";", False), (STRING, True), (NO_OP, True), ('CdhCore.cmdDisp.CMD_NO_OP_STRING, "unfinished', False), (NO_OP, True)],
    [(TEST, True), ("Nope.cmd.DOES_NOT_EXIST", False), (TEST, True), ("CdhCore.cmdDisp.CMD_TEST_CMD_1, 5, 1e3, 7", False), (TEST, True),
     ("CdhCore.cmdDisp.CMD_TEST_CMD_1, 6", False), ("CdhCore.cmdDisp.CMD_TEST_CMD_1, 6, 1.5, 7", True)],
]


def enter(page, selector, text):
    page.click(selector)
    page.keyboard.press("Control+a")
    page.keyboard.press("Delete")
    page.keyboard.type(text)
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
            for typed, whole in sequence:
                enter(page, "input[name=command-text]", typed)
                shown = page.evaluate(line.PYTHON_SHOWN)
                page.click("button:has-text('Send Command')")
                case.append({"typed": typed, "whole": whole, "line": shown["line"], "name": shown["name"],
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

    different, sent = [], 0
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            page = harness.open_page(browser, gds.url, [])
            for case in cases:
                page.reload()
                page.wait_for_selector("body[data-ready=true]")
                harness.show(page, "commands")
                for step in case:
                    where = ([s["typed"] for s in case], step["typed"])
                    enter(page, "#command-string", step["typed"])
                    shown = page.evaluate(line.NEW_SHOWN)
                    shown["inputs"], wanted = line.plain(shown["inputs"]), {**step, "inputs": line.plain(step["inputs"])}
                    # With another command shown, the form of the line's command is not on screen.
                    if step["whole"] or shown["name"] == step["name"]:
                        for key in ("line", "name") + (("inputs",) if step["whole"] else ()):
                            if shown[key] != wanted[key]:
                                different.append((*where, f"{key}: Python GDS page {wanted[key]!r}, new page {shown[key]!r}"))
                    before = last()
                    page.click("#command-send")
                    try:
                        scenario.wait_for("the command", lambda: last() != before, 2 if step["whole"] else 1)
                        raw = base64.b64decode(last()[0]["raw"]).hex()
                    except TimeoutError:
                        raw = None
                    if step["whole"]:
                        sent += 1
                        if raw is None or raw not in step["frame"]:
                            different.append((*where, f"Python GDS page sent {step['frame']}, new page {raw}"))
                    elif raw is not None:
                        different.append((*where, f"a line that is not a command, the new page sent {raw}"))
            browser.close()
    finally:
        gds.stop()
    for typed, at, text in different:
        print(f"DIFFERENT {typed} at {at!r}: {text}")
    if different:
        sys.exit(f"{len(different)} differences from the Python GDS page")
    print(f"  ok: {len(cases)} sequences of refused and taken Command String lines, each then the send button: {sent} "
          "taken lines send the command packet of the Python GDS page's frame, the others send nothing")


if __name__ == "__main__":
    if sys.argv[1] == "record":
        record(sys.argv[2], int(sys.argv[3]), sys.argv[4])
    else:
        check(*sys.argv[2:6])
