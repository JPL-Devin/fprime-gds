#!/usr/bin/env python3
"""The browser UI against a running Ref: what a user sees and does in each tab.

Usage: ui.py <fgds binary> <browser-gds binary> <Ref binary> <dictionary> <directory for logs and screenshots>

The stock binary is what every check runs, except the one of a tab that fails: that tab is in browser-gds.
"""
import base64
import json
import urllib.parse
import os
import re
import shutil
import struct
import time
import subprocess
import sys
from email.utils import parsedate_to_datetime

from playwright.sync_api import expect, sync_playwright

import subprocess

import harness
from harness import rows, scenario, show, step

BINARY, BROKEN_BINARY, REF, DICTIONARY, OUT = sys.argv[1:6]
os.makedirs(OUT, exist_ok=True)
# In the order of the Python GDS page, then the tab it does not have.
TABS = {"commands": "Commanding", "events": "Events", "channels": "Channels", "uplink": "Uplink", "downlink": "Downlink",
        "dictionary": "Dictionaries", "charts": "Charts", "logs": "Logs", "sequences": "Sequences", "advanced": "Advanced",
        "data-products": "Data Products"}
ELEMENTS = {"commands": "commands-tab", "events": "events-tab", "channels": "channels-tab",
            "dictionary": "dictionary-tab", "logs": "logs-tab", "advanced": "advanced-tab",
            "uplink": "fgds-uplink", "downlink": "fgds-downlink", "data-products": "data-products-tab",
            "charts": "charts-tab", "sequences": "fgds-sequences"}
HTML = '<img src=x onerror=injected=1><b>x</b>'  # within the 40 characters of the argument
NO_OP_STRING = "CdhCore.cmdDisp.CMD_NO_OP_STRING"
COUNTER = "CdhCore.cmdDisp.CommandsDispatched"


def choose(page, name):
    page.select_option("#command-select", name)
    expect(page.locator("#command-name")).to_contain_text(name)


def events_named(page, name):
    return [r for r in rows(page, "#events-table") if r["name"].endswith(name)]


def counter(page):
    found = [r for r in rows(page, "#channels-table") if r["name"] == COUNTER]
    return int(found[0]["value"]) if found else None


def stock(browser, gds):
    problems = []
    page = harness.open_page(browser, gds.url, problems)

    # Each tab loads: its button, its element, and content from the API.
    titles = page.eval_on_selector_all("#tabs button", "all => all.map((b) => [b.dataset.tab, b.textContent])")
    assert list(map(tuple, titles)) == list(TABS.items()), titles
    content = {"commands": "#command-select option", "events": "#events-table tbody tr",
               "channels": "#channels-table tbody tr", "dictionary": "#dictionary-table tbody tr",
               "logs": "#logs-select option", "advanced": "#advanced-plugins tbody tr",
               "uplink": "#files", "downlink": "table", "data-products": "table.products",
               "charts": ".chart-channel option", "sequences": "#sequence-text"}
    for tab in TABS:
        show(page, tab)
        page.wait_for_selector(f"{ELEMENTS[tab]} {content[tab]}", state="attached")
        assert page.locator(".error-panel").count() == 0
    expect(page.locator("#status")).to_contain_text("connected")
    step(f"each tab loads and shows data: {', '.join(TABS.values())}; the status bar reports the link connected")

    # Events and channels are followed from the moment their tab is first shown.
    show(page, "channels")
    page.wait_for_function("() => document.querySelector('channels-tab').dataset.history === 'loaded'")
    page.wait_for_function(f"""() => [...document.querySelectorAll('#channels-table td[data-col=name]')]
        .some((cell) => cell.textContent === '{COUNTER}')""")
    before = counter(page)

    # `channels_display_last_received` of the Advanced tab: ticked at first; cleared, a sample with a later time
    # is still shown. (A sample with an earlier time is not sent by Ref: that part of the rule is not checked here.)
    show(page, "advanced")
    assert page.is_checked("#channels-display-last-received")
    page.uncheck("#channels-display-last-received")
    show(page, "channels")
    newest = "Math.max(...[...document.querySelectorAll('#channels-table tbody tr')].map((row) => Number(row.dataset.seq)))"
    seen = page.evaluate(newest)
    page.wait_for_function(f"() => {newest} > {seen}")
    show(page, "advanced")
    page.check("#channels-display-last-received")
    show(page, "channels")
    before = counter(page)
    step("Advanced: the channels_display_last_received box is ticked at first; cleared, a later sample is still shown")

    # A command with no argument, sent from the Commanding tab, produces its events.
    show(page, "commands")
    choose(page, "CdhCore.cmdDisp.CMD_NO_OP")
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("CMD_NO_OP sent")
    show(page, "events")
    page.wait_for_function("""() => ['NoOpReceived', 'OpCodeCompleted'].every((name) =>
        [...document.querySelectorAll('#events-table td[data-col=name]')].some((cell) => cell.textContent.endsWith(name)))""")
    received = events_named(page, "NoOpReceived")[-1]
    assert received["text"] == "Received a NO-OP command" and received["severity"] == "ACTIVITY_HI", received
    colour = page.eval_on_selector("#events-table tr.sev-ACTIVITY_HI", "(row) => getComputedStyle(row).backgroundColor")
    assert colour == "rgb(128, 177, 211)", colour
    step(f"CMD_NO_OP sent from the Commanding tab; the Events tab shows {received['name']} "
         f"({received['severity']}, {received['text']!r}, {received['time']}) and the completion, in the severity colour")

    # Typed inputs: a wrong value is refused in the page; an enum argument is a list of its constants.
    show(page, "commands")
    choose(page, "CdhCore.cmdDisp.CMD_TEST_CMD_1")
    page.fill("[data-arg=arg1]", "not a number")
    page.fill("[data-arg=arg2]", "2.5")
    page.fill("[data-arg=arg3]", "300")
    sent = len(rows(page, "#command-history"))
    page.click("#command-send")
    expect(page.locator("[data-error-for=arg1]")).to_be_visible()
    expect(page.locator("[data-error-for=arg3]")).to_be_visible()
    assert len(rows(page, "#command-history")) == sent, "an invalid command was sent"
    # The inputs take and refuse what the Python GDS page takes and refuses, with its messages.
    signed = "Supply signed decimal integer  between -2147483648 and 2147483647"
    unsigned = "Supply binary, octal, decimal, or hexadecimal unsigned integer  between 0 and 255"
    # "0x10", "007" and " 4 " for a signed integer, "007" and " 4 " for an unsigned one, and no text: the Python
    # GDS page marks the field and sends nothing, without a message (its validate.js reads them with BigInt; the
    # pattern of the input refuses them).
    cases = [("arg1", "-5", None), ("arg1", "0", None), ("arg1", "0x10", ""), ("arg1", "007", ""),
             ("arg1", " 4 ", ""), ("arg1", "", ""), ("arg1", "2147483648", signed), ("arg1", "abc", signed),
             ("arg3", "0b101", None), ("arg3", "0o17", None), ("arg3", "0x10", None), ("arg3", "255", None),
             ("arg3", "007", ""), ("arg3", "256", unsigned), ("arg3", "-1", unsigned), ("arg3", " 4 ", "")]
    assert page.get_attribute("[data-arg=arg2]", "type") == "number", "a float is a number input"
    for name, text, message in cases:
        for key, good in (("arg1", "1"), ("arg2", "-2.5"), ("arg3", "1")):
            page.fill(f"[data-arg={key}]", text if key == name else good)
        sent = len(rows(page, "#command-history"))
        page.click("#command-send")
        if message is None:
            scenario.wait_for(f"{name}={text!r} sent", lambda: len(rows(page, "#command-history")) == sent + 1, 10)
        else:
            assert "invalid" in page.get_attribute(f"[data-arg={name}]", "class"), (name, text)
            if message:
                assert page.text_content(f"[data-error-for={name}]") == message, (name, text)
                assert page.locator("#command-message").count() == 0, "a line under the form and a message"
            else:
                # No message in the Python GDS page (the browser explains): the field is marked, nothing more.
                assert page.locator(f"[data-error-for={name}]").count() == 0, (name, text)
            assert len(rows(page, "#command-history")) == sent, f"{name}={text!r} was sent"
    step(f"command inputs: {len(cases)} integer texts taken or refused as in the Python GDS page, with its messages; "
         "a negative float is sent")
    # Every form of number the Python GDS page takes, typed key by key; what is compared is the packet sent.
    def last_command():
        return gds.get("/records?kind=fprime.command&limit=10000")["records"][-1]

    def typed(values):
        """Type each text into its input, send, and return the bytes of the arguments (None when refused)."""
        before = last_command()["seq"]
        for key, text in values.items():
            page.fill(f'[data-arg="{key}"]', "")
            page.locator(f'[data-arg="{key}"]').press_sequentially(text)
        shown = {key: page.input_value(f'[data-arg="{key}"]') for key in values}
        page.click("#command-send")
        try:
            scenario.wait_for("the command", lambda: last_command()["seq"] != before, 2)
        except TimeoutError:
            return shown, None
        return shown, base64.b64decode(last_command()["raw"])

    floats = [("1e3", 1000.0), ("1E3", 1000.0), ("-2.5e-3", -0.0025), ("1e-2", 0.01), (".5", 0.5), ("-.5", -0.5),
              ("-0", -0.0), ("3.25", 3.25), ("-17", -17.0), ("1.5e+2", 150.0)]
    for text, value in floats:
        shown, raw = typed({"arg1": "1", "arg2": text, "arg3": "5"})
        assert shown["arg2"] == text, f"{text!r} typed, the field shows {shown['arg2']!r}"
        assert raw is not None and raw.endswith(struct.pack(">ifB", 1, value, 5)), (text, raw and raw.hex())
    integers = [("arg3", "0b101", 5), ("arg3", "0B11", 3), ("arg3", "0o17", 15), ("arg3", "0O7", 7), ("arg3", "0x10", 16),
                ("arg3", "0XfF", 255), ("arg3", "200", 200), ("arg1", "-2147483648", -2147483648), ("arg1", "17", 17)]
    for name, text, value in integers:
        shown, raw = typed({"arg1": "1", "arg2": "2", "arg3": "5", name: text})
        expected = {"arg1": 1, "arg3": 5, name: value}
        assert raw is not None and raw.endswith(struct.pack(">ifB", expected["arg1"], 2.0, expected["arg3"])), \
            (name, text, raw and raw.hex())
    # A text that is not a number, in a float field: the field stays empty and nothing is sent.
    for text in ("abc", "e", "1e", "--1", "-Infinity", "1e400", "NaN", "-inf"):
        # A valid command first: the message of the text before is gone.
        assert typed({"arg1": "1", "arg2": "2", "arg3": "5"})[1] is not None and not page.query_selector("[data-error-for=arg2]")
        shown, raw = typed({"arg1": "1", "arg2": text, "arg3": "5"})
        assert raw is None, f"{text!r} was sent as {raw.hex()}"
        assert page.text_content("[data-error-for=arg2]") == "Supply floating point number", text
    choose(page, "Ref.typeDemo.SEND_SCALARS")
    members = {"i8": "-8", "i16": "-16", "i32": "-32", "i64": "-64", "u8": "0x8", "u16": "0b10000", "u32": "0o40",
               "u64": "64", "f32": "2.5e1", "f64": "-2.5e-3"}
    shown, raw = typed({f"scalar_input.{name}": text for name, text in members.items()})
    assert raw is not None and raw.endswith(struct.pack(">bhiqBHIQfd", -8, -16, -32, -64, 8, 16, 32, 64, 25.0, -0.0025)), \
        raw and raw.hex()
    # A member out of range: its own message, and the sentence the Python page writes under the struct.
    shown, raw = typed({f"scalar_input.{name}": text for name, text in {**members, "u64": "18446744073709551616"}.items()})
    assert raw is None, raw and raw.hex()
    assert page.text_content("[data-error-for='scalar_input.u64']") == \
        "Supply binary, octal, decimal, or hexadecimal unsigned integer  between 0 and 18446744073709551615"
    assert page.text_content("[data-error-for=scalar_input]") == "Error in field/index: u64."
    shown, raw = typed({"scalar_input.u64": "64", "scalar_input.i8": "200", "scalar_input.f32": "x"})
    assert raw is None and page.text_content("[data-error-for=scalar_input]") == \
        "Error in field/index: i8. Error in field/index: f32.", page.text_content("[data-error-for=scalar_input]")
    step(f"numbers typed key by key: {len(floats)} float texts (exponent, sign, leading dot), {len(integers)} integer "
         "texts (binary, octal, hexadecimal, decimal) and the ten members of SEND_SCALARS are sent with the bytes "
         "of the value; 4 texts that are not numbers are refused")
    # The frames the Python GDS page sent to Ref for the same steps; the command packet is inside.
    untouched = "20440425001000c0000018000005002003000000000000000a6e6f66696c652e62696efff9be"
    kept = "2044041a001000c020000d00001000500300000000000000017513"
    page.reload()
    page.wait_for_selector("body[data-ready=true]")
    show(page, "commands")
    choose(page, "FileHandling.fileManager.RemoveFile")
    assert page.input_value("[data-arg=ignoreErrors]") == "", "a Boolean nobody touched has a value"
    shown, raw = typed({"fileName": "nofile.bin"})
    assert raw is None, "RemoveFile was sent with a Boolean nobody chose"
    page.select_option("[data-arg=ignoreErrors]", "true")
    assert page.input_value("#command-string") == 'FileHandling.fileManager.RemoveFile, "nofile.bin", true'
    shown, raw = typed({"fileName": "nofile.bin"})
    assert raw is not None and raw.hex() in untouched, raw and raw.hex()
    for text, elements in (("Ref.typeDemo.CHOICES, [ONE]", (0, 0)), ("Ref.typeDemo.CHOICES, [ONE, TWO]", (0, 1)),
                           ("Ref.typeDemo.CHOICES, [ONE]", (0, 1))):
        page.fill("#command-string", text)
        page.press("#command-string", "Tab")
        shown, raw = typed({})
        assert raw is not None and raw.endswith(struct.pack(">ii", *elements)), (text, raw and raw.hex())
    assert raw.hex() in kept, raw.hex()
    step("a Boolean nobody touched has no value and nothing is sent; chosen, RemoveFile is sent with its bytes; "
         "`CHOICES, [ONE]` sends [ONE, ONE] on a new form and [ONE, TWO] after [ONE, TWO] was sent, its bytes")
    choose(page, "CdhCore.events.SET_EVENT_FILTER")
    options = page.eval_on_selector("select[data-arg=filterLevel]", "(s) => [...s.options].map((o) => o.value)")
    assert "WARNING_HI" in options and "ACTIVITY_LO" in options, options
    page.select_option("select[data-arg=filterLevel]", "ACTIVITY_LO")
    page.select_option("select[data-arg=filterEnabled]", "ENABLED")
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("SET_EVENT_FILTER sent")
    step(f"CMD_TEST_CMD_1 with a text for an I32 and 300 for a U8 is refused in the page; "
         f"SET_EVENT_FILTER sent with enum inputs ({', '.join(options)})")

    # A text longer than the type holds is refused; an empty text is a value and is sent.
    choose(page, NO_OP_STRING)
    page.fill("[data-arg=arg1]", "x" * 60)
    sent = len(rows(page, "#command-history"))
    page.click("#command-send")
    assert page.text_content("[data-error-for=arg1]") == "Supply general text of less than 40 characters"
    assert len(rows(page, "#command-history")) == sent, "a string of 60 characters was sent"
    page.fill("[data-arg=arg1]", "")
    page.click("#command-send")
    scenario.wait_for("the empty string in the history", lambda: len(rows(page, "#command-history")) == sent + 1, 10)
    assert rows(page, "#command-history")[0]["string"] == f'{NO_OP_STRING}, ""'
    step("CMD_NO_OP_STRING: 60 characters are refused; an empty text is sent")
    # A string that contains HTML is shown as text everywhere it appears.
    choose(page, NO_OP_STRING)
    page.fill("[data-arg=arg1]", HTML)
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("CMD_NO_OP_STRING sent")
    history = rows(page, "#command-history")
    assert history[0]["string"] == f'{NO_OP_STRING}, "{HTML}"', history[0]
    show(page, "events")
    page.wait_for_function("""() => [...document.querySelectorAll('#events-table td[data-col=name]')]
        .some((cell) => cell.textContent.endsWith('NoOpStringReceived'))""")
    # The event of the empty string sent above may be the last one for a moment.
    scenario.wait_for(
        "the event of the string that contains HTML",
        lambda: events_named(page, "NoOpStringReceived")[-1]["text"] == f"Received a NO-OP string={HTML}",
        10,
    )
    show(page, "logs")
    page.select_option("#logs-select", "fprime.event")
    expect(page.locator("#logs-text")).to_contain_text(HTML)
    injected = page.evaluate("""() => [window.injected === undefined,
        document.querySelectorAll('#tab img, #tab b, #tab script').length]""")
    assert injected == [True, 0], injected
    step(f"the string {HTML!r} is shown as text in the command history, the Events tab and the Logs tab; nothing ran")

    # Load and resend from the command history.
    show(page, "commands")
    page.click("#command-history tbody tr:first-child button[data-action=load]")
    assert page.input_value("[data-arg=arg1]") == HTML
    page.click("#command-history tbody tr:first-child button[data-action=resend]")
    page.wait_for_function(f"() => document.querySelectorAll('#command-history tbody tr').length === {len(history) + 1}")
    step("command history: a row loads into the form and is sent again")

    # A command the GDS refuses (a float beyond F32) has a row with its name and its arguments, as the Python page.
    refused = "CdhCore.cmdDisp.CMD_TEST_CMD_1"
    page.fill("#command-string", f"{refused}, 1, 2.5, 1")
    page.press("#command-string", "Tab")
    page.fill("[data-arg=arg2]", "1e39")
    sent = len(rows(page, "#command-history"))
    page.click("#command-send")
    scenario.wait_for("the refused command in the history", lambda: len(rows(page, "#command-history")) == sent + 1, 10)
    row = rows(page, "#command-history")[0]
    assert row["string"] == f"{refused}, 1, 1e+39, 1" and row["outcome"].startswith("rejected"), row
    step(f"a refused command ({refused} with 1e39 for an F32) has a history row: {row['string']!r}")

    # A Command String that is not a command with its arguments, after a command was loaded and sent: the send
    # button sends nothing (no command record in the GDS, no command counted by Ref), and the page says so.
    # The history row loaded shows in the command list too.
    # (A line that leaves arguments out and names the command shown is sent with what the form holds: refused.py.)
    page.fill("#command-string", "CdhCore.cmdDisp.CMD_TEST_CMD_1, 5, 1.5, 7")
    page.press("#command-string", "Tab")
    assert page.input_value("#command-select") == "CdhCore.cmdDisp.CMD_TEST_CMD_1", page.input_value("#command-select")
    commands = lambda: len(gds.get("/records?kind=fprime.command&limit=10000")["records"])
    dispatched = lambda: next(r["body"]["value"] for r in gds.get("/records/latest?kind=fprime.channel")["records"]
                              if r["body"].get("name") == COUNTER)
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("CMD_TEST_CMD_1 sent")
    # The counter of Ref comes with its telemetry, some time after the commands: wait until it is still for 5 s.
    held = (commands(), scenario.steady(dispatched, quiet=5.0))
    for text in ("", "   ", 'CdhCore.cmdDisp.CMD_NO_OP_STRING, "unfinished', "CdhCore.cmdDisp.CMD_TEST_CMD_1, 5, , 7",
                 "Nope.cmd.DOES_NOT_EXIST", "CdhCore.cmdDisp.CMD_NO_OP_STRING", "FileHandling.fileManager.RemoveFile, a"):
        choose(page, "CdhCore.cmdDisp.CMD_TEST_CMD_1")
        for key, value in (("arg1", "5"), ("arg2", "1.5"), ("arg3", "7")):
            page.fill(f"[data-arg={key}]", value)
        page.fill("#command-string", text)
        page.press("#command-string", "Tab")
        assert page.locator("#command-send").count() == 1, f"{text!r}: no send button"
        page.click("#command-send")
        page.wait_for_timeout(1500)
        assert commands() == held[0], f"{text!r} then the send button: a command went out"
        assert page.locator("#command-message.error-text, #command-string-error, #command-unknown").count(), f"{text!r}: refused in silence"
    after = (commands(), scenario.steady(dispatched, quiet=5.0))
    assert after == held, f"a command went out: {held} before, {after} after"
    step("Command String empty, of spaces, not of the form, of an unknown name, with an argument that is not one, then the send "
         "button: nothing is sent and the page says why")
    # Finding 54: more arguments than the command takes. Refused as on the command line, with its message;
    # nothing is sent and the line stays as typed (the Python page sends the command without the last one).
    for name, line, takes in (
            ("CdhCore.cmdDisp.CMD_NO_OP", "CdhCore.cmdDisp.CMD_NO_OP, 5", 0),
            ("CdhCore.cmdDisp.CMD_NO_OP_STRING", 'CdhCore.cmdDisp.CMD_NO_OP_STRING, "a", 5', 1),
            ("CdhCore.cmdDisp.CMD_TEST_CMD_1", "CdhCore.cmdDisp.CMD_TEST_CMD_1, 5, 1.5, 7, 8", 3),
            ("Ref.typeDemo.CHOICES", 'Ref.typeDemo.CHOICES, ["ONE", "TWO"], 5', 1),
            ("Ref.typeDemo.CHOICE_PAIR", 'Ref.typeDemo.CHOICE_PAIR, {"firstChoice": "ONE", "secondChoice": "TWO"}, 5', 1)):
        for enter in (True, False):
            choose(page, name)
            page.fill("#command-string", line)
            page.press("#command-string", "Enter" if enter else "Tab")
            expect(page.locator("#command-string-error")).to_have_text(f"{name} takes {takes} arguments, {takes + 1} given")
            assert page.input_value("#command-string") == line
            page.click("#command-send")
            page.wait_for_timeout(1000)
            assert commands() == held[0], f"{line!r} then the send button: a command went out"
            assert page.locator("#command-message.error-text, #command-string-error").count(), f"{line!r}: refused in silence"
    assert (commands(), scenario.steady(dispatched, quiet=5.0)) == held
    step("Command String with one argument more than the command takes (0, 1, 3, an array, a structure), then the send "
         "button: `<name> takes N arguments, N+1 given`, nothing is sent")
    choose(page, "CdhCore.cmdDisp.CMD_NO_OP")
    page.click("#command-history tbody tr:has-text('CMD_TEST_CMD_1') button[data-action=load] >> nth=0")
    assert page.input_value("#command-select") == "CdhCore.cmdDisp.CMD_TEST_CMD_1", "the list does not show the row loaded"
    assert page.eval_on_selector_all("#command-form select[data-arg] option", "(all) => all.length") == 0
    choose(page, "FileHandling.fileManager.RemoveFile")
    options = page.eval_on_selector_all("#command-form select[data-arg] option", "(all) => all.map((o) => o.textContent)")
    assert options == ["True", "False"], options
    step("a history row loaded is the one selected in the command list; a boolean is chosen from True and False")
    # The search of the command list reads the names only, and its text goes when a command is shown (finding 50).
    page.fill("#command-filter", "0x1000000")
    assert page.inner_text("#command-none") == "Sorry, no matching options.", "an opcode is searched"
    box = page.locator("#command-none").bounding_box()
    assert page.is_visible("#command-none") and box["width"] > 50 and box["height"] > 8, f"the text is not on screen: {box}"
    assert page.evaluate("""(b) => document.elementFromPoint(b.x + 5, b.y + b.height / 2).closest('#command-none') !== null""", box), \
        "the text is covered"
    page.screenshot(path=f"{gds.out}/no-matching-options.png")
    assert page.eval_on_selector_all("#command-select option", "(all) => all.length") == 0
    page.fill("#command-filter", "zzz")
    page.click("#command-history tbody tr:has-text('CMD_TEST_CMD_1') button[data-action=load] >> nth=0")
    assert page.input_value("#command-filter") == "", "the search text stays after a history row is loaded"
    assert page.input_value("#command-select") == "CdhCore.cmdDisp.CMD_TEST_CMD_1", "nothing is selected after the load"
    page.fill("#command-filter", "no_op_str")
    assert page.eval_on_selector_all("#command-select option", "(all) => all.map((o) => o.value)") == ["CdhCore.cmdDisp.CMD_NO_OP_STRING"]
    page.fill("#command-filter", "")
    step("command list search: names only (`0x1000000`: `Sorry, no matching options.`); the text is cleared when a history row is loaded")

    # One U8 left empty and one holding 300: the message under each, in one press.
    choose(page, "Ref.typeDemo.CHOICES_WITH_FRIENDS")
    fields = page.locator("#command-form [data-arg]")
    kinds = fields.evaluate_all("(all) => all.map((e) => e.tagName)")
    numbers = [i for i, kind in enumerate(kinds) if kind == "INPUT"]
    assert len(numbers) == 2, kinds
    fields.nth(numbers[1]).fill("300")
    page.click("#command-send")
    said = page.eval_on_selector_all("#command-form span[data-error-for]", "(all) => all.map((e) => e.textContent.trim())")
    u8 = "Supply binary, octal, decimal, or hexadecimal unsigned integer  between 0 and 255"
    assert said == [u8, u8], said
    step("CHOICES_WITH_FRIENDS with one U8 empty and the other 300: the message of the Python GDS page under both")

    # "Show Parameter Commands": ticked at start; without it the _PRM_SET and _PRM_SAVE commands leave the list,
    # but for the one selected.
    listed = lambda: page.eval_on_selector_all("#command-select option", "(all) => all.map((o) => o.value)")
    parameter = lambda names: [n for n in names if n.endswith("_PRM_SET") or n.endswith("_PRM_SAVE")]
    assert page.is_checked("#show-parameter-commands")
    everything = listed()
    assert len(parameter(everything)) >= 2, everything
    choose(page, parameter(everything)[0])
    page.uncheck("#show-parameter-commands")
    assert listed() == [n for n in everything if n not in parameter(everything)[1:]], listed()
    choose(page, NO_OP_STRING)
    assert parameter(listed()) == [] and len(listed()) == len(everything) - len(parameter(everything))
    page.check("#show-parameter-commands")
    assert listed() == everything
    step(f"Show Parameter Commands: ticked at start; unticked, the {len(parameter(everything))} _PRM_SET and _PRM_SAVE "
         "commands leave the list, but for the one selected")

    # Channel values update without a reload.
    show(page, "channels")
    page.wait_for_function(f"""() => [...document.querySelectorAll('#channels-table tbody tr')].some((row) =>
        row.querySelector('[data-col=name]').textContent === '{COUNTER}'
        && Number(row.querySelector('[data-col=value]').textContent) >= {before + 4})""")
    step(f"Channels tab: {COUNTER} went from {before} to {counter(page)} while the page stayed open")

    # The page shows each event once, and the same events as the record API.
    show(page, "events")
    api = [r["seq"] for r in gds.get("/records?kind=fprime.event&limit=10000")["records"]]
    page.wait_for_function(f"() => document.querySelectorAll('#events-table tbody tr').length === {len(api)}")
    shown = page.eval_on_selector_all("#events-table tbody tr", "all => all.map((row) => row.dataset.seq)")
    assert sorted(shown, key=int) == api, "the Events tab repeats or misses a record"
    step(f"Events tab: {len(api)} events, the same as the record API, none repeated or missing")

    # Pause holds the table; resume shows what arrived meanwhile.
    page.click("#events-pause")
    gds.post("/commands", {"name": "CdhCore.cmdDisp.CMD_NO_OP", "args": []})
    scenario.wait_for("the events", lambda: len(gds.get("/records?kind=fprime.event&limit=10000")["records"]) >= len(api) + 2)
    assert page.locator("#events-table tbody tr").count() == len(api)
    page.click("#events-pause")
    page.wait_for_function(f"() => document.querySelectorAll('#events-table tbody tr').length >= {len(api) + 2}")
    page.fill("#events-filter", "NoOpStringReceived")
    assert {r["name"].split(".")[-1] for r in rows(page, "#events-table")} == {"NoOpStringReceived"}
    page.fill("#events-filter", "")
    step("Events tab: pause holds the table, resume shows the events that arrived, the filter narrows it")

    # As the tables of the Python GDS page: a click on a header sorts by the text of that column, ascending,
    # then descending, then as received; each column has a box that hides it.
    def column(table, key):
        return page.eval_on_selector_all(f"#{table} tbody td[data-col={key}]", "(all) => all.map((c) => c.textContent)")
    ordered = "(texts) => texts.slice().sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))"
    for tab, table, keys in (("events", "events-table", ["time", "ground", "id", "name", "severity", "text"]),
                             ("channels", "channels-table", ["time", "ground", "id", "name", "value"])):
        show(page, tab)
        if tab == "events":
            page.click("#events-pause")
        for key in keys:
            received = column(table, key)
            page.click(f"#{table} th[data-col={key}]")
            up = column(table, key)
            # The Python page sorts the description and the value by what it holds, not by the text shown
            # (a command name is an element, a structure an object): parity.py compares those orders with it.
            held = key in ("text", "value")
            # Channels keep arriving: the texts read after the click are compared with themselves in order.
            assert held or up == page.evaluate(ordered, received if tab == "events" else up), f"{tab} {key}: not ascending"
            page.click(f"#{table} th[data-col={key}]")
            down = column(table, key)
            if tab == "channels":
                up = page.evaluate(ordered, down)
            assert held or down == up[::-1] or len(set(up)) < len(up), f"{tab} {key}: not descending"
            assert tab == "channels" or sorted(column(table, key)) == sorted(up)  # channels keep arriving
            page.click(f"#{table} th[data-col={key}]")
            if tab == "events":
                assert column(table, key) == received, f"{tab} {key}: the order received is not back"
            page.uncheck(f"#{tab}-{key}")
            assert not page.locator(f"#{table} th[data-col={key}]").is_visible()
            assert not page.locator(f"#{table} tbody td[data-col={key}]").first.is_visible()
            page.check(f"#{tab}-{key}")
            assert page.locator(f"#{table} th[data-col={key}]").is_visible()
        if tab == "events":
            page.click("#events-pause")
    step("Events and Channels tables: each column sorts by a click on its header (ascending, descending, as received) "
         "and is hidden and shown by its box")

    # "Edit View" of the Channels tab, as the Python page: a box per channel, Clear, Save, Load, Done; with a view,
    # only its channels are listed.
    show(page, "channels")
    every = page.locator("#channels-table tbody tr").count()
    page.click("#view-edit")
    assert page.locator("#channels-table tbody .in-view").count() > every  # channels without a value too
    assert [page.locator(f"#{i}").count() for i in ("view-clear", "view-save", "view-load", "view-done")] == [1] * 4
    page.fill("#channels-filter", "CommandsDispatched")
    page.locator("#channels-table tbody .in-view").first.check()
    page.fill("#channels-filter", "BD_Cycles")
    page.check("#view-all")
    assert page.get_attribute("#view-save", "href").endswith(
        urllib.parse.quote("CdhCore.cmdDisp.CommandsDispatched\nRef.blockDrv.BD_Cycles", safe=""))
    page.click("#view-done")
    assert [r["name"] for r in rows(page, "#channels-table")] == ["CdhCore.cmdDisp.CommandsDispatched", "Ref.blockDrv.BD_Cycles"]
    page.click("#view-edit")
    page.click("#view-clear")
    page.click("#view-done")
    assert page.locator("#channels-table tbody tr").count() >= every
    step("Channels tab, Edit View: two channels ticked (one by the box of the header) are the only rows after Done; "
         "Save holds their names; Clear brings every channel back")

    # The top bar: the data-flow indicator is on while events or channels arrive; the new-window button opens the page again.
    expect(page.locator("#data-flow")).to_have_attribute("data-flow", "on", timeout=10000)
    with page.context.expect_page() as opened:
        page.click("#new-window")
    # As the Python GDS page, the address names the tab as its button does, and such an address opens that tab.
    assert opened.value.url == page.url == f"{gds.url}/#Channels", opened.value.url
    opened.value.wait_for_selector("#tabs button[data-tab=channels][aria-selected=true]")
    names = {}
    for button in opened.value.locator("#tabs button[data-tab]").all():
        button.click()
        names[button.get_attribute("data-tab")] = opened.value.url.split("#")[1]
    python = ["Commanding", "Events", "Channels", "Uplink", "Downlink", "Dictionaries", "Charts", "Logs", "Sequences", "Advanced"]
    assert [name for name in names.values() if name != "Data%20Products"] == python, names
    for tab, name in names.items():
        for fragment in (name, tab):
            opened.value.goto(f"{gds.url}/")
            opened.value.goto(f"{gds.url}/#{fragment}")
            opened.value.wait_for_selector(f"#tabs button[data-tab={tab}][aria-selected=true]")
    opened.value.goto("about:blank")
    opened.value.goto(f"{gds.url}/")
    opened.value.wait_for_selector("#tabs button[data-tab=commands][aria-selected=true]")
    assert opened.value.url == f"{gds.url}/", opened.value.url
    opened.value.close()
    step("top bar: the data-flow indicator is on while Ref sends; the new-window button opens the page in a new window; "
         "the address names each tab as the Python GDS page (#Channels), and opens the tab it names")

    # The counters of the top bar, and "Clear All": as the Python GDS page.
    severities = [r["body"]["severity"] for r in gds.get("/records?kind=fprime.event&limit=10000")["records"]]
    expected = [f"WARNING HI: {severities.count('WARNING_HI')}", f"FATAL: {severities.count('FATAL')}", "GDS Errors: 0"]
    scenario.wait_for("the counters", lambda: page.locator("#counters span").all_text_contents() == expected)
    show(page, "events")
    page.click("#events-clear")
    assert page.locator("#events-table tbody tr").count() == 0
    gds.post("/commands", {"name": "CdhCore.cmdDisp.CMD_NO_OP", "args": []})
    page.wait_for_function("() => document.querySelectorAll('#events-table tbody tr').length >= 2")
    assert len(gds.get("/records?kind=fprime.event&limit=10000")["records"]) >= len(severities) + 2
    show(page, "channels")
    assert page.locator("#channels-table tbody tr").count() > 0
    left = page.evaluate("""async () => {
        document.querySelector('#channels-clear').click();
        await document.querySelector('channels-tab').updateComplete;
        return document.querySelectorAll('#channels-table tbody tr').length; }""")
    assert left == 0, left
    page.wait_for_function(f"""() => [...document.querySelectorAll('#channels-table td[data-col=name]')]
        .some((cell) => cell.textContent === {COUNTER!r})""")
    step(f"top bar: {', '.join(expected)}; Clear All empties the Events and Channels tables of the page, which then show "
         "what arrives; the GDS keeps its records")

    # The page takes nothing from another origin.
    other = []
    late = browser.new_page()
    late.on("request", lambda request: other.append(request.url) if not request.url.startswith(gds.url) else None)
    late.goto(gds.url)
    late.wait_for_selector("body[data-ready=true]")
    for tab in TABS:
        show(late, tab)
    policy = late.request.get(gds.url).headers["content-security-policy"]
    assert not other and "default-src 'self'" in policy, (other, policy)
    late.close()
    step(f"every request of the page goes to the GDS itself; policy: {policy}")

    files_and_products(page, gds)
    charts_and_sequences(page, gds, problems)
    stream_that_ends(browser, gds)

    for tab, title in TABS.items():
        show(page, tab)
        page.wait_for_timeout(300)
        page.screenshot(path=f"{OUT}/{title.lower().replace(' ', '-')}.png")
    assert not problems, problems
    step(f"no script error in the page; screenshots in {OUT}")
    page.close()


def files_and_products(page, gds):
    """The Uplink, Downlink and Data Products tabs, with Ref on the other side."""
    content = bytes(range(256)) * 9 + b"the end"
    with open(f"{gds.out}/up.bin", "wb") as file:
        file.write(content)
    show(page, "uplink")
    uplink = page.locator("fgds-uplink")
    expect(uplink).to_contain_text("link: connected")
    # As the Python GDS page: a chosen file waits as NOT STARTED, nothing is sent before "Begin Uplink".
    # The staged row and the pause button as the Python page: the Destination Folder text of the moment the file
    # was added, `0%`, and `Unpause` as long as the GDS has no file.
    uplink.locator("#directory").fill(" some/dir ")
    uplink.locator("#files").set_input_files(f"{gds.out}/up.bin")
    expect(uplink.locator("tr.staged", has_text="up.bin")).to_contain_text("NOT STARTED")
    assert uplink.locator("th").all_text_contents() == [
        "Start Time", "End Time", "Source", "Destination", "State", "Control", "Progress"]
    staged_cells = [c.strip() for c in uplink.locator("tr.staged td").all_text_contents()]
    assert [" ".join(c.split()) for c in staged_cells] == [
        "", "", "up.bin", "some/dir", "NOT STARTED", "Remove Select Packets", "0%"], staged_cells
    # "Select Packets", as the Python page: a list of packet numbers from 1, with its message for anything else.
    uplink.locator("tr.staged .select-packets").click()
    expect(uplink.locator("#packet-selection h3")).to_have_text("Uplink File Packet List")
    uplink.locator("#packetList").fill("1, 0")
    expect(uplink.locator("#packet-selection .error")).to_have_text("Specify packet numbers as comma-separated list")
    assert uplink.locator("#packets-save").is_disabled()
    uplink.locator("#packetList").fill("2, 1")
    uplink.locator("#packets-save").click()
    expect(uplink.locator("tr.staged td").nth(2)).to_have_text("up.bin [2, 1]")
    uplink.locator("tr.staged .select-packets").click()
    uplink.locator("#packetList").fill("")
    uplink.locator("#packets-save").click()
    expect(uplink.locator("tr.staged td").nth(2)).to_have_text("up.bin")
    assert uplink.locator("#unpause").count() == 1 and uplink.locator("#pause").count() == 0
    uplink.locator("#directory").fill("")
    page.wait_for_timeout(1000)
    assert gds.get("/files/uplink")["files"] == [] and not os.path.exists(f"{gds.out}/ref/up.bin")
    uplink.locator("tr.staged").get_by_role("button", name="Remove").click()
    expect(uplink.locator("tr.staged")).to_have_count(0)
    uplink.locator("#files").set_input_files(f"{gds.out}/up.bin")
    expect(uplink.locator("#cancel-packet")).to_be_enabled()
    uplink.get_by_role("button", name="Begin Uplink").click()
    # As the Python page: "Send Cancel Packet" is disabled while a file is being sent.
    expect(uplink.locator("#cancel-packet")).to_be_disabled(timeout=10000)
    expect(uplink.locator("tr", has_text="up.bin")).to_contain_text("finished", timeout=30000)
    expect(uplink.locator("#cancel-packet")).to_be_enabled()
    expect(uplink.locator("tr.staged")).to_have_count(0)
    expect(uplink.locator("tr", has_text="up.bin")).to_contain_text(f"{len(content)} / {len(content)} bytes")
    expect(uplink.locator("#pause")).to_have_count(1)
    received = scenario.wait_for("the file on the Ref side", lambda: os.path.getsize(f"{gds.out}/ref/up.bin") == len(content))
    assert received and open(f"{gds.out}/ref/up.bin", "rb").read() == content
    # Times and destination as the Python GDS page writes them: "Sat, 21 Nov 2026 22:53:23 GMT"; the name alone
    # without a directory, and directory, "/", name with one (`os.path.join` in its uplinker).
    cells = uplink.locator("tr", has_text="up.bin").locator("td").all_text_contents()
    http_date = re.compile(r"^(Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT$")
    assert http_date.match(cells[0]) and http_date.match(cells[1]), cells
    assert parsedate_to_datetime(cells[0]) <= parsedate_to_datetime(cells[1]), cells
    assert cells[3] == "up.bin", cells
    # What the Python page shows and Ref receives for each text of its Destination Folder field
    # (`os.path.join` in its uplinker), read from that page and from `Ref`'s FileReceived event.
    os.makedirs(f"{gds.out}/ref/dir", exist_ok=True)
    joined = [(".", "./"), ("dir", "dir/"), ("dir/", "dir/"), ("dir//", "dir//"), ("./dir", "./dir/"), ("", "")]
    for index, (directory, prefix) in enumerate(joined):
        name = f"up{index + 2}.bin"
        shutil.copy(f"{gds.out}/up.bin", f"{gds.out}/{name}")
        uplink.locator("#files").set_input_files(f"{gds.out}/{name}")
        uplink.locator("#directory").fill(directory)
        uplink.get_by_role("button", name="Begin Uplink").click()
        expect(uplink.locator("tr", has_text=name)).to_contain_text("finished", timeout=30000)
        assert uplink.locator("tr", has_text=name).locator("td").all_text_contents()[3] == prefix + name, directory
        scenario.wait_for(f"Ref received {prefix}{name}", lambda: any(
            r["body"]["display_text"] == f"Received file {prefix}{name}"
            for r in gds.get("/records?kind=fprime.event&after_seq=0&limit=10000")["records"]))
    uplink.locator("#directory").fill("")
    # Newest first, as the Python page (its list is the uplinked files reversed).
    order = [row.locator("td").nth(3).text_content() for row in uplink.locator("tr:has(td)").all()]
    assert order == ["up7.bin", "./dir/up6.bin", "dir//up5.bin", "dir/up4.bin", "dir/up3.bin", "./up2.bin", "up.bin"], order
    # The filter field and the sorting headers of the tables of the Python page.
    destinations = lambda: [row.locator("td").nth(3).text_content() for row in uplink.locator("tr:has(td)").all()]
    uplink.locator("#filter").fill("DIR/")
    assert destinations() == ["./dir/up6.bin", "dir//up5.bin", "dir/up4.bin", "dir/up3.bin"], destinations()
    uplink.locator("#filter").fill("")
    uplink.locator("th", has_text="Destination").click()
    ascending = page.evaluate("(texts) => texts.sort((a, b) => a.localeCompare(b, undefined, { numeric: true }))", order)
    assert ascending != order and destinations() == ascending, destinations()
    uplink.locator("th", has_text="Destination").click()
    assert destinations() == ascending[::-1], destinations()
    uplink.locator("th", has_text="Destination").click()
    assert destinations() == order, destinations()
    step("Uplink tab: the filter field keeps the rows holding the text; a click on a header sorts ascending, a second descending, a third as listed")
    # N9-1: the name is sent as given. Outside ASCII: a notice; a space: none. Ref cuts the name in its event
    # at 40 bytes, here inside a character: the event is shown with the replacement character.
    spaced, accented = "n9 space.txt", "n9-ab" + "\u00e9" * 30 + ".bin"
    for name in (spaced, accented):
        shutil.copy(f"{gds.out}/up.bin", f"{gds.out}/{name}")
        uplink.locator("#files").set_input_files(f"{gds.out}/{name}")
        uplink.get_by_role("button", name="Begin Uplink").click()
        expect(uplink.locator("tr", has_text=name)).to_contain_text("finished", timeout=30000)
        assert uplink.locator("tr", has_text=name).locator("td").all_text_contents()[3] == name
        expect(uplink.locator(".uplink-notice")).to_have_count(0 if name == spaced else 1)
        assert open(f"{gds.out}/ref/{name}", "rb").read() == content
    expect(uplink.locator(".uplink-notice")).to_contain_text("outside ASCII")
    show(page, "events")
    expect(page.locator("events-tab")).to_contain_text("Received file n9-ab" + "\u00e9" * 17 + "\ufffd")
    show(page, "uplink")
    step("Uplink tab: `n9 space.txt` and a 69-byte name outside ASCII reach Ref under these names; the second gives a notice; "
         "its FileReceived event, cut by Ref inside a character, is shown on the Events tab")
    # A file name that holds HTML is shown as text: staged, in the queue, and in the event of Ref.
    name = "<img src=x onerror=injected=1><b>x.bin"
    shutil.copy(f"{gds.out}/up.bin", f"{gds.out}/{name}")
    uplink.locator("#files").set_input_files(f"{gds.out}/{name}")
    expect(uplink.locator("tr.staged")).to_contain_text(name)
    uplink.get_by_role("button", name="Begin Uplink").click()
    expect(uplink.locator("tr", has_text=name)).to_contain_text("finished", timeout=30000)
    assert uplink.locator("tr", has_text=name).locator("td").all_text_contents()[3] == name
    assert open(f"{gds.out}/ref/{name}", "rb").read() == content
    show(page, "events")
    expect(page.locator("events-tab")).to_contain_text(f"Received file {name}")
    show(page, "uplink")
    injected = page.evaluate("""() => [window.injected === undefined,
        document.querySelectorAll('#tab img, #tab b, #tab script').length]""")
    assert injected == [True, 0], injected
    step(f"Uplink tab: the file name {name!r} is shown as text when staged, in the queue and in the event; nothing ran")
    # F10-4: one selection of `..`, good1.txt, good2.txt. The refused file is named with the reason and leaves
    # the staged rows; the two others reach Ref; a second Begin Uplink posts nothing.
    payload = lambda name: {"name": name, "mimeType": "application/octet-stream", "buffer": content}
    uplink.locator("#files").set_input_files([payload(".."), payload("good1.txt"), payload("good2.txt")])
    expect(uplink.locator("tr.staged")).to_have_count(3)
    posts = []
    page.on("request", lambda request: posts.append(request.url) if "/files/uplink?" in request.url else None)
    uplink.get_by_role("button", name="Begin Uplink").click()
    for name in ("good1.txt", "good2.txt"):
        expect(uplink.locator("tr", has_text=name)).to_contain_text("finished", timeout=30000)
        scenario.wait_for(f"Ref received {name}", lambda: open(f"{gds.out}/ref/{name}", "rb").read() == content)
    expect(uplink.locator("tr.staged")).to_have_count(0)
    expect(uplink.locator(".uplink-refused")).to_have_count(1)
    refusal = uplink.locator(".uplink-refused").text_content()
    assert "REFUSED" in refusal and '".."' in refusal and "not a usable file name" in refusal, refusal
    uplink.get_by_role("button", name="Begin Uplink").click()
    page.wait_for_timeout(500)
    assert len(posts) == 3, posts
    uplink.locator(".uplink-refused button").click()
    expect(uplink.locator(".uplink-refused")).to_have_count(0)
    step(f"Uplink tab: `..`, good1.txt and good2.txt chosen together: the first is refused with its reason ({' '.join(refusal.split())!r}) "
         "and leaves the list, the two others reach Ref; Begin Uplink again posts nothing")
    # G3 on the page: a backslash is part of the file name, not a separator (the Python page writes `ab.bin`).
    uplink.locator("#files").set_input_files([payload("a\\b.bin")])
    expect(uplink.locator("tr.staged")).to_contain_text("a\\b.bin")
    uplink.get_by_role("button", name="Begin Uplink").click()
    expect(uplink.locator("tr", has_text="a\\b.bin")).to_contain_text("finished", timeout=30000)
    scenario.wait_for("Ref received a\\b.bin", lambda: open(f"{gds.out}/ref/a\\b.bin", "rb").read() == content)
    assert not os.path.exists(f"{gds.out}/ref/b.bin") and not os.path.exists(f"{gds.out}/ref/ab.bin")
    step("Uplink tab: `a\\b.bin` is written by Ref under its whole name")
    # Three files in the queue, as the Python GDS: the start time is when the file begins, the end time when its
    # end packet is sent, and the next file begins at once (the three start times of the Python page are equal).
    queued = ["queued1.bin", "queued2.bin", "queued3.bin"]
    for name in queued:
        shutil.copy(f"{gds.out}/up.bin", f"{gds.out}/{name}")
    uplink.locator("#files").set_input_files([f"{gds.out}/{name}" for name in queued])
    uplink.get_by_role("button", name="Begin Uplink").click()
    for name in queued:
        expect(uplink.locator("tr", has_text=name)).to_contain_text("finished", timeout=30000)
    times = [[parsedate_to_datetime(text) for text in uplink.locator("tr", has_text=name).locator("td").all_text_contents()[:2]]
             for name in queued]
    for (_, ended), (started, _) in zip(times, times[1:]):
        assert 0 <= (started - ended).total_seconds() <= 1, times
    for name in queued:
        scenario.wait_for(f"Ref received {name}", lambda: open(f"{gds.out}/ref/{name}", "rb").read() == content)
    uplink.locator("th", has_text="Start Time").click()
    # Rows of the same second keep the order of the list, newest first (a neighbour may share it).
    starts = [parsedate_to_datetime(row.locator("td").nth(0).text_content()) for row in uplink.locator("tr:has(td)").all()]
    assert starts == sorted(starts) and "up.bin" in destinations()[:2] and set(queued) <= set(destinations()[-4:]), destinations()
    uplink.locator("th", has_text="Start Time").click()
    uplink.locator("th", has_text="Start Time").click()
    step(f"Uplink tab: three files of the queue begin one after the other with no wait ({[str(t[0].time()) for t in times]}); "
         "sorted by Start Time the rows are in the order of their times")
    step(f"Uplink tab: up.bin ({len(content)} bytes) chosen in the page, sent at once and finished; Ref wrote the same bytes; "
         f"times as the Python page ({cells[0]}); destination shown and received by Ref as the Python page for the "
         f"directories {[d for d, _ in joined]}")

    # A file and a data product come down: the Downlink tab lists both, the directory of
    # downlinked files is the watched directory of the data products.
    show(page, "downlink")
    downlink = page.locator("fgds-downlink")
    gds.post("/commands", {"name": "FileHandling.fileDownlink.SendFile", "args": ["down-source.bin", "down.bin"]})
    expect(downlink.locator("tr", has_text="down.bin").first).to_contain_text("finished", timeout=30000)
    # One link in the Control column of the row, as the Python page, and one in the table of files.
    expect(downlink.locator("a[download='down.bin']")).to_have_count(2)
    assert downlink.locator("table").first.locator("th").all_text_contents() == [
        "Start Time", "End Time", "Source", "Destination", "State", "Control", "Progress", "Problems"]
    # A part of a file ends as finished with its percentage and no problem counted, as on the Python GDS page.
    gds.post("/commands", {"name": "FileHandling.fileDownlink.SendPartial", "args": ["down-source.bin", "part.bin", 100, 500]})
    expect(downlink.locator("tr", has_text="part.bin").first).to_contain_text("finished", timeout=30000)
    part = downlink.locator("tr", has_text="part.bin").first.locator("td").all_text_contents()
    assert http_date.match(part[0]) and http_date.match(part[1]) and part[4] == "finished" and part[5] == "Download" and part[7] == "", part
    assert re.search(r"\(\d+%\)", part[6]) and "(100%)" not in part[6], part
    sources = lambda: downlink.locator("#transfers tr:has(td) td:nth-child(3)").all_text_contents()
    listed = sources()
    downlink.locator("#filter").fill("part")
    assert sources() and all("part" in text for text in downlink.locator("#transfers tr:has(td)").all_text_contents()), sources()
    downlink.locator("#filter").fill("")
    downlink.locator("#transfers th", has_text="Source").click()
    assert sorted(sources()) == sorted(listed) and sources()[0] == min(listed), sources()
    for _ in range(2):
        downlink.locator("#transfers th", has_text="Source").click()
    assert sources() == listed, sources()
    step("Downlink tab: the filter field and the sorting headers, as the Uplink tab")
    with page.expect_download() as download:
        downlink.locator("tr", has_text="down.bin").first.locator("td").nth(5).locator("a").click()
    assert open(download.value.path(), "rb").read() == open(f"{gds.out}/ref/down-source.bin", "rb").read()
    step("Downlink tab: down.bin sent by Ref is listed as finished, and the file downloaded from the page is the one Ref sent")

    gds.post("/commands", {"name": "DataProducts.dpCat.BUILD_CATALOG", "args": []})
    page.wait_for_timeout(1000)
    gds.post("/commands", {"name": "Ref.dpDemo.Dp", "args": ["IMMEDIATE", 10, "PROC_TYPE_NONE"]})
    product = scenario.wait_for("a data product on the Ref side",
                                lambda: sorted(f for f in os.listdir(f"{gds.out}/ref/DpCat") if f.endswith(".fdp")))[0]
    page.wait_for_timeout(1000)
    gds.post("/commands", {"name": "DataProducts.dpCat.START_XMIT_CATALOG", "args": ["NO_WAIT", False]})
    expect(downlink.locator("tr", has_text=product).first).to_contain_text("finished", timeout=30000)
    show(page, "data-products")
    products = page.locator("data-products-tab")
    row = products.locator("table.products tbody tr")
    expect(row).to_have_count(1, timeout=30000)
    row.click()
    expect(products.locator(".panel table.tree tr").first).to_be_visible(timeout=30000)
    assert products.locator(".bad").count() == 0, products.locator(".bad").all_text_contents()
    shown = products.locator(".panel").inner_text()
    step(f"Data Products tab: {product} downlinked by Ref appears in the watched directory; its detail opens, "
         f"decoded ({products.locator('.panel table.tree tr').count()} lines, {len(shown)} characters)")


def chart_views(page, charts, canvas):
    """Zoom, pan, Reset Zoom, Lock Scale and collapse of the Python GDS page, each done with the pointer on two charts."""
    def view(index):
        chart = canvas.nth(index)
        low, high, bottom, top = (float(chart.get_attribute(f"data-{name}")) for name in ("from", "to", "low", "high"))
        return {"from": low, "to": high, "width": high - low, "height": top - bottom, "low": bottom, "zoomed": chart.get_attribute("data-zoomed")}

    def settled(what, condition):
        scenario.wait_for(what, condition)
        return view(0), view(1)

    def at(index, across, down):
        box = canvas.nth(index).bounding_box()
        return box["x"] + box["width"] * across, box["y"] + box["height"] * down

    def wheel(index, across, down, notches):
        page.mouse.move(*at(index, across, down))
        page.keyboard.down("Alt")
        for _ in range(abs(notches)):
            page.mouse.wheel(0, -100 if notches > 0 else 100)
        page.keyboard.up("Alt")

    def drag(index, key, start, end):
        page.mouse.move(*at(index, *start))
        page.keyboard.down(key)
        page.mouse.down()
        page.mouse.move(*at(index, *end), steps=6)
        page.mouse.up()
        page.keyboard.up(key)

    near = lambda value, wanted, part=0.03: abs(value - wanted) <= part * abs(wanted)
    canvas.nth(0).scroll_into_view_if_needed()
    first, second = view(0), view(1)
    assert near(first["width"], 60000) and near(second["width"], 60000) and first["zoomed"] == "false", (first, second)
    # The wheel without ALT changes nothing.
    page.mouse.move(*at(0, 0.5, 0.5))
    page.mouse.wheel(0, -100)
    page.mouse.wheel(0, 100)
    assert view(0)["zoomed"] == "false"

    wheel(0, 0.5, 0.5, 3)
    zoomed, other = settled("the chart zoomed in by ALT and the wheel", lambda: near(view(0)["width"], 60000 * 0.9 ** 3))
    assert near(zoomed["height"], first["height"] * 0.9 ** 3) and zoomed["zoomed"] == "true", (first, zoomed)
    assert near(other["width"], 60000) and other["zoomed"] == "false", f"the other chart changed without Lock Scale: {other}"
    wheel(0, 0.5, 0.5, -3)
    out, _ = settled("the chart zoomed out again by ALT and the wheel", lambda: near(view(0)["width"], 60000 * 0.9 ** 3 * 1.1 ** 3))
    assert near(out["height"], first["height"] * 0.9 ** 3 * 1.1 ** 3), (first, out)
    wheel(0, 0.5, 0.97, 2)  # over the time axis
    axis, _ = settled("the time range alone zoomed over the time axis", lambda: near(view(0)["width"], out["width"] * 0.81))
    assert near(axis["height"], out["height"], 0.001), (out, axis)

    charts.locator(".chart-reset").first.click()
    again, _ = settled("the first view after Reset Zoom", lambda: view(0)["zoomed"] == "false" and near(view(0)["width"], 60000))
    assert near(again["height"], first["height"], 0.001) and abs(again["to"] - time.time() * 1000) < 3000, (first, again)

    drag(0, "Shift", (0.4, 0.4), (0.6, 0.6))
    moved, other = settled("the chart panned by SHIFT and a drag", lambda: view(0)["zoomed"] == "true")
    behind = time.time() * 1000 - moved["to"]
    assert near(moved["width"], 60000) and 7000 < behind < 20000, f"a drag over a fifth of the chart moves the time range by {behind} ms"
    assert near(moved["height"], first["height"], 0.001) and moved["low"] > first["low"], (first, moved)
    assert other["zoomed"] == "false", other
    time.sleep(1.5)
    assert near(time.time() * 1000 - view(0)["to"], behind, 0.1), "the panned range no longer scrolls with the clock"
    charts.locator(".chart-reset").first.click()
    settled("the first view after Reset Zoom", lambda: view(0)["zoomed"] == "false")

    drag(0, "Alt", (0.3, 0.3), (0.6, 0.7))
    chosen, _ = settled("the chart zoomed into the selection", lambda: view(0)["zoomed"] == "true")
    assert 0.25 * 60000 < chosen["width"] < 0.45 * 60000 and 0.3 * first["height"] < chosen["height"] < 0.65 * first["height"], (first, chosen)
    assert charts.locator(".chart-selection").count() == 0, "the selection rectangle stayed"
    charts.locator(".chart-reset").first.click()
    settled("the first view after Reset Zoom", lambda: view(0)["zoomed"] == "false")

    lock = charts.locator("#chart-lock")
    assert lock.get_attribute("aria-pressed") == "false"
    lock.click()
    assert lock.get_attribute("aria-pressed") == "true"
    wheel(1, 0.5, 0.5, 4)
    one, two = settled("both charts on the time range zoomed in the second", lambda: near(view(1)["width"], 60000 * 0.9 ** 4) and near(view(0)["width"], 60000 * 0.9 ** 4))
    assert abs(one["from"] - two["from"]) < 500 and abs(one["to"] - two["to"]) < 500, (one, two)
    drag(0, "Shift", (0.4, 0.5), (0.6, 0.5))
    one, two = settled("both charts on the time range panned in the first", lambda: time.time() * 1000 - view(1)["to"] > 4000)
    assert abs(one["from"] - two["from"]) < 500 and abs(one["to"] - two["to"]) < 500, (one, two)
    pause = charts.locator(".chart-pause")
    pause.first.click()
    expect(pause.nth(1)).to_have_text("Resume")
    # The frame under way when Pause is pressed may still be drawn: the view is read after it.
    time.sleep(0.5)
    stopped = view(1)
    time.sleep(1)
    assert view(1)["to"] == stopped["to"], "the other chart went on after Pause with Lock Scale"
    pause.nth(1).click()
    expect(pause.first).to_have_text("Pause")
    charts.locator(".chart-reset").nth(1).click()
    settled("the first view of both charts after Reset Zoom with Lock Scale", lambda: view(0)["zoomed"] == "false" and view(1)["zoomed"] == "false")
    lock.click()
    assert lock.get_attribute("aria-pressed") == "false"
    wheel(1, 0.5, 0.5, 1)
    one, two = settled("the second chart alone zoomed after Lock Scale is off", lambda: view(1)["zoomed"] == "true")
    assert one["zoomed"] == "false" and near(one["width"], 60000), one
    charts.locator(".chart-reset").nth(1).click()

    collapse = charts.locator(".chart-collapse")
    expect(charts.locator(".chart-title").first).to_have_text(COUNTER)
    collapse.first.click()
    expect(canvas.first).to_be_hidden()
    expect(charts.locator(".chart-channel").first).to_be_hidden()
    expect(collapse.first).to_have_text("Expand")
    expect(canvas.nth(1)).to_be_visible()
    points = int(canvas.first.get_attribute("data-points"))
    collapse.first.click()
    expect(canvas.first).to_be_visible()
    assert int(canvas.first.get_attribute("data-points")) >= points > 0, "the samples of the collapsed chart are lost"
    scenario.wait_for("the chart drawn again after Expand", lambda: page.evaluate("""() => { const c = document.querySelector("charts-tab canvas");
        const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
        let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i]) n += 1; return n; }""") > 100)


def charts_and_sequences(page, gds, problems):
    """The Charts tab plots a channel as it arrives; the Sequences tab builds a sequence and sends it."""
    show(page, "charts")
    charts = page.locator("charts-tab")
    offered = charts.locator(".chart-channel").first.locator("option").all_text_contents()[1:]
    channels = sorted(gds.get("/dictionary")["telemetryChannels"], key=lambda c: c["id"])
    owner = lambda entry: max((c for c in channels if entry == c["name"] or entry.startswith(c["name"] + ".")),
                              key=lambda c: len(c["name"]))["id"]
    identifiers = [owner(entry) for entry in offered]
    assert len(offered) == 187 and identifiers == sorted(identifiers), (len(offered), offered[:5])
    assert {"ComCcsds.comQueue.comQueueDepth.0", "ComCcsds.comQueue.comQueueDepth.1", COUNTER} <= set(offered), offered
    before = [r for r in gds.records("fprime.channel") if r["body"]["name"] == COUNTER]
    assert before, "the counter has samples before the chart"
    charts.locator(".chart-channel").first.select_option(COUNTER)
    canvas = charts.locator("canvas")
    expect(canvas).to_have_count(1)
    points = lambda: int(canvas.get_attribute("data-points") or 0)
    time.sleep(1)
    assert points() == 0, f"{points()} points before any sample arrived"
    gds.post("/commands", {"name": "CdhCore.cmdDisp.CMD_NO_OP", "args": []})
    scenario.wait_for("a point in the chart", points)
    after = [r for r in gds.records("fprime.channel") if r["body"]["name"] == COUNTER][len(before):]
    assert float(canvas.get_attribute("data-first")) == float(after[0]["body"]["value"]), (canvas.get_attribute("data-first"), after[0])
    start = float(canvas.get_attribute("data-from"))
    assert abs(time.time() * 1000 - 60000 - start) < 5000, start
    drawn = page.evaluate("""() => { const c = document.querySelector("charts-tab canvas");
        const d = c.getContext("2d").getImageData(0, 0, c.width, c.height).data;
        let n = 0; for (let i = 3; i < d.length; i += 4) if (d[i]) n += 1; return n; }""")
    assert drawn > 100, drawn
    depth = lambda: [r for r in gds.records("fprime.channel") if r["body"]["name"] == "ComCcsds.comQueue.comQueueDepth"]
    known = len(depth())
    # The chart box changes size when its bottom right corner is dragged, as the Help text says.
    box = canvas.first.bounding_box()
    page.mouse.move(box["x"] + box["width"] - 3, box["y"] + box["height"] - 3)
    page.mouse.down()
    page.mouse.move(box["x"] + box["width"] - 103, box["y"] + box["height"] + 77, steps=5)
    page.mouse.up()
    scenario.wait_for("the chart drawn in its new size", lambda: (
        abs(canvas.first.bounding_box()["height"] - box["height"] - 80) < 12
        and abs(canvas.first.bounding_box()["width"] - box["width"] + 100) < 12))
    charts.locator("#chart-add").click()
    charts.locator(".chart-channel").nth(1).select_option("ComCcsds.comQueue.comQueueDepth.1")
    expect(canvas).to_have_count(2)
    scenario.wait_for("a point of the array element", lambda: int(canvas.nth(1).get_attribute("data-points") or 0))
    assert float(canvas.nth(1).get_attribute("data-first")) in [float(r["body"]["value"][1]) for r in depth()[known - 1:]]
    chart_views(page, charts, canvas)
    charts.locator(".chart-remove").first.click()
    expect(canvas).to_have_count(1)
    assert charts.locator(".chart-channel").first.input_value() == "ComCcsds.comQueue.comQueueDepth.1"
    # Load: one chart per known name of the file, each selection list showing the channel its chart plots.
    listed = ["Ref.blockDrv.BD_Cycles", COUNTER, "Not.a.Channel", "Ref.typeDemo.ChoiceCh", "CdhCore.version.FrameworkVersion"]
    with open(f"{gds.out}/charts.txt", "w") as file:
        file.write("\n".join(listed) + "\n")
    charts.locator("#chart-load").set_input_files(f"{gds.out}/charts.txt")
    expect(canvas).to_have_count(3)
    shown = lambda: page.evaluate('() => [...document.querySelectorAll("charts-tab .chart-channel")].map((s) => s.selectedOptions[0].textContent)')
    gds.post("/commands", {"name": "CdhCore.cmdDisp.CMD_NO_OP", "args": []})
    scenario.wait_for("a point in the loaded chart of the counter", lambda: int(canvas.nth(2).get_attribute("data-points") or 0))
    counted = [float(r["body"]["value"]) for r in gds.records("fprime.channel") if r["body"]["name"] == COUNTER]
    assert float(canvas.nth(2).get_attribute("data-first")) in counted, "the loaded chart of the counter plots another channel"
    assert shown() == ["ComCcsds.comQueue.comQueueDepth.1", *listed[:2]], f"selection lists after Load: {shown()}"
    assert charts.locator(".chart-title").all_text_contents() == shown(), "titles and selection lists differ after Load"
    assert page.evaluate('() => [...document.querySelectorAll("charts-tab canvas")].map((c) => Chart.getChart(c).data.datasets[0].label)') == shown()
    expect(charts.locator("#chart-unknown")).to_have_text("Not loaded, no such channel in the dictionary: Not.a.Channel")
    expect(charts.locator("#chart-uncharted")).to_have_text(
        "Not loaded, a channel that is not a number cannot be charted: Ref.typeDemo.ChoiceCh, CdhCore.version.FrameworkVersion")
    for _ in range(2):
        charts.locator(".chart-remove").last.click()
    expect(canvas).to_have_count(1)
    assert shown() == ["ComCcsds.comQueue.comQueueDepth.1"], f"selection list after two charts are removed: {shown()}"
    step("Charts Load: a chart per known channel of the file, its selection list and title naming the channel plotted; "
         "the unknown name gets no chart and is named in a line, an enumeration channel and a text channel in another line")
    step(f"Charts tab: {len(offered)} entries in identifier order, array elements by index; {COUNTER} drawn with the "
         f"vendored Chart.js from the first sample after it was chosen ({len(before)} earlier ones are not drawn), "
         "on an axis that ends at the present; a second chart of an array element; ALT and the wheel zoom (both ranges over "
         "the plot, the time range alone over the time axis), ALT and a dragged selection zoom into it, SHIFT and a drag "
         "pan, Reset Zoom gives the first view back, Lock Scale gives both charts one time range, pause and reset, "
         "Collapse hides the chart and Expand shows it again")

    show(page, "sequences")
    tab = page.locator("fgds-sequences")
    text = "; two commands\nR00:00:00 CdhCore.cmdDisp.CMD_NO_OP\nR00:00:01 CdhCore.cmdDisp.CMD_NO_OP_STRING \"from a sequence\"\n"
    assert tab.locator("#sequence-text").input_value() == "" and not tab.locator("#sequence-run").is_enabled()
    staged = gds.get("/files/uplink")["directory"]
    expect(tab.locator("#sequence-name-error")).to_have_text("Supply filename ending with .seq")
    tab.locator("#sequence-uplink").click()
    # As the Python page: the answer to the request takes the place of the message of the field.
    expect(tab.locator("#sequence-name-error")).to_have_text("Supply filename with .seq suffix")
    tab.locator("#sequence-name").fill("page.seq")
    # A directory of the flight side; `Ref` runs in a directory of this check, and a command takes 40 characters.
    tab.locator("#sequence-directory").fill(".")
    tab.locator("#sequence-text").fill("R00:00:00 Not.a.COMMAND\n")
    tab.locator("#sequence-uplink").click()
    wrong = f"{staged}/page.seq:1: 'Not.a.COMMAND' does not match any command in the command dictionary."
    expect(tab.locator("#sequence-result")).to_have_value(wrong)
    refused = wrong
    # Round 10, F10-2: a wrong number of arguments is refused as by `fgds seqgen` and `command-send`, with the
    # line and the command; nothing is built or uplinked.
    uplinks = len(gds.get("/files/uplink")["files"])
    tab.locator("#sequence-text").fill("R00:00:00 CdhCore.cmdDisp.CMD_NO_OP\nR00:00:01 CdhCore.cmdDisp.CMD_TEST_CMD_1\n"
                                       "R00:00:02 CdhCore.cmdDisp.CMD_NO_OP 1, 2\n")
    tab.locator("#sequence-uplink").click()
    expect(tab.locator("#sequence-result")).to_have_value(
        f"{staged}/page.seq:2: CdhCore.cmdDisp.CMD_TEST_CMD_1 takes 3 arguments, 0 given\n"
        f"{staged}/page.seq:3: CdhCore.cmdDisp.CMD_NO_OP takes 0 arguments, 2 given")
    assert len(gds.get("/files/uplink")["files"]) == uplinks
    # The browser logs the refusal of the route; it is the one error expected in the console.
    assert [p for p in problems if "422" not in p] == [], problems
    problems.clear()
    with page.expect_file_chooser() as chooser:
        tab.locator("#sequence-open").click()
    chooser.value.set_files({"name": "opened.seq", "mimeType": "text/plain", "buffer": text.encode()})
    expect(tab.locator("#sequence-name")).to_have_value("opened.seq")
    assert tab.locator("#sequence-text").input_value() == text
    tab.locator("#sequence-name").fill("page.seq")
    with page.expect_download() as download:
        tab.locator("#sequence-save").click()
    assert download.value.suggested_filename == "page.seq" and open(download.value.path()).read() == text
    tab.locator("#sequence-uplink").click()
    expect(tab.locator("#sequence-result")).to_have_value(re.compile("Please confirm"))
    said = tab.locator("#sequence-result").input_value()
    with open(f"{gds.out}/page.seq", "w") as file:
        file.write(text)
    subprocess.run([BINARY, "seqgen", "--dictionary", DICTIONARY, f"{gds.out}/page.seq", f"{gds.out}/page.tool.bin"],
                   check=True, capture_output=True)
    built = open(f"{gds.out}/page.tool.bin", "rb").read()
    scenario.wait_for("the sequence on the Ref side",
                      lambda: os.path.exists(f"{gds.out}/ref/page.bin") and os.path.getsize(f"{gds.out}/ref/page.bin") == len(built))
    assert open(f"{gds.out}/ref/page.bin", "rb").read() == built
    scenario.wait_for("the uplink finished", lambda: [t for t in gds.get("/files/uplink")["files"]
                                                     if t["name"] == "page.bin" and t["state"] == "finished"])
    crc = struct.unpack(">I", built[-4:])[0]
    assert said == (f"Sequence is {len(built) - 15} bytes with timebase ANY\nCRC: {crc} (0x{crc:04X})\n"
                    "Uplinking to ./page.bin. Please confirm uplink EVRs before running.\n"), said
    before = len(gds.records("fprime.event"))
    tab.locator("#sequence-run").click()
    expect(tab.locator("#sequence-result")).to_have_value("Sent CS_RUN for ./page.bin")
    scenario.wait_for("the sequence ran", lambda: [e for e in gds.records("fprime.event")[before:]
                                                   if e["body"]["name"] == "Ref.cmdSeq.CS_SequenceComplete"])
    ran = [e["body"]["name"].rsplit(".", 1)[1] for e in gds.records("fprime.event")[before:]]
    assert "NoOpStringReceived" in ran, ran
    step(f"Sequences tab: a sequence with an unknown command is refused ({refused.strip()[:60]!r}), one with a command given too few or too many arguments is refused naming line and command; one opened from a file is saved, built "
         f"({len(built)} bytes, equal to `fgds seqgen`), uplinked with the messages of the Python GDS page, and run with "
         f"the Run button: {', '.join(ran)}")


def stream_that_ends(browser, gds):
    """A subscription whose server ends it continues after the last record delivered (UI-3)."""
    page = browser.new_page()
    # The stream of kind `test.stream` is played by the page itself: the first socket delivers a
    # record, says records were dropped and closes; the second delivers the next record.
    page.add_init_script("""
        window.opened = [];
        const Real = window.WebSocket;
        window.WebSocket = function (url) {
            if (!url.includes("kind=test.stream")) return new Real(url);
            const socket = { close() { setTimeout(() => socket.onclose && socket.onclose({}), 0); } };
            const say = (data) => socket.onmessage && socket.onmessage({ data: JSON.stringify(data) });
            const nth = window.opened.push(url.split("?")[1]);
            setTimeout(() => {
                if (nth === 1) {
                    say({ type: "record", record: { seq: "5" } });
                    say({ type: "lagged", last_seq: "5" });
                    socket.close();
                } else if (nth === 2) say({ type: "record", record: { seq: "6" } });
            }, 0);
            return socket;
        };
    """)
    page.goto(gds.url)
    page.wait_for_selector("body[data-ready=true]")
    page.evaluate("""() => { window.seen = [];
        window.end = gds.records.subscribe({ kinds: ["test.stream"], afterSeq: "3" }, (r) => window.seen.push(r.seq)); }""")
    page.wait_for_function("() => window.seen.length === 2", timeout=10000)
    opened = page.evaluate("window.opened")
    assert page.evaluate("window.seen") == ["5", "6"], page.evaluate("window.seen")
    assert opened == ["kind=test.stream&after_seq=3", "kind=test.stream&after_seq=5"], opened
    page.evaluate("() => window.end()")
    page.wait_for_timeout(1500)
    assert len(page.evaluate("window.opened")) == 2  # ended by the tab: no reconnection
    step("a stream the server ends is opened again after the last record delivered, "
         "not when the tab ended it")
    page.close()


def broken(browser, gds):
    problems = []
    page = harness.open_page(browser, gds.url, problems)
    show(page, "broken")
    panel = page.locator(".error-panel:visible")
    expect(panel).to_contain_text("Broken")
    expect(panel.locator(".plugin")).to_have_text("broken")
    expect(panel).to_contain_text("fails on purpose")
    page.screenshot(path=f"{OUT}/broken.png")
    for tab in TABS:
        show(page, tab)
        expect(page.locator(ELEMENTS[tab])).to_be_visible()
    show(page, "events")
    count = page.locator("#events-table tbody tr").count()
    gds.post("/commands", {"name": "CdhCore.cmdDisp.CMD_NO_OP", "args": []})
    page.wait_for_function(f"() => document.querySelectorAll('#events-table tbody tr').length >= {count + 2}")
    step("a tab that fails shows an error panel naming its plugin; the other tabs load and the Events tab stays live")
    page.close()


def run(binary, check):
    out = f"{OUT}/{check.__name__}"
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(f"{out}/ref/DpCat")  # where Ref runs: its files and its data products
    with open(f"{out}/ref/down-source.bin", "wb") as file:
        file.write(bytes(range(256)) * 20)
    gds = harness.Gds(binary, DICTIONARY, out, extra=[
        "--files-directory", f"{out}/files", "--data-products-dir", f"{out}/files/downlink"])
    gds.out = out
    ref = None
    try:
        ref = scenario.start([REF, "-a", "127.0.0.1", "-p", str(gds.link_port)], open(f"{out}/ref.log", "w"), cwd=f"{out}/ref")
        scenario.wait_for("telemetry from Ref", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])
        check(browser, gds)
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)


with sync_playwright() as playwright:
    browser = playwright.chromium.launch()
    run(BINARY, stock)
    run(BROKEN_BINARY, broken)
    browser.close()
print("browser ui: all checks passed")
