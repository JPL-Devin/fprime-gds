#!/usr/bin/env python3
"""The browser UI against a running Ref: what a user sees and does in each tab.

Usage: ui.py <fgds binary> <browser-gds binary> <Ref binary> <dictionary> <directory for logs and screenshots>

The stock binary is what every check runs, except the one of a tab that fails: that tab is in browser-gds.
"""
import base64
import json
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
            assert page.text_content(f"[data-error-for={name}]") == message, (name, text)
            assert "invalid" in page.get_attribute(f"[data-arg={name}]", "class"), (name, text)
            assert page.locator("#command-message").count() == 0 or not page.text_content("#command-message"), \
                "a line under the form, which the Python GDS page does not have"
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
    for text in ("abc", "e", "1e", "--1"):
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
    choose(page, "CdhCore.events.SET_EVENT_FILTER")
    options = page.eval_on_selector("select[data-arg=filterLevel]", "(s) => [...s.options].map((o) => o.value)")
    assert "WARNING_HI" in options and "ACTIVITY_LO" in options, options
    page.select_option("select[data-arg=filterLevel]", "ACTIVITY_LO")
    page.select_option("select[data-arg=filterEnabled]", "ENABLED")
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("SET_EVENT_FILTER sent")
    step(f"CMD_TEST_CMD_1 with a text for an I32 and 300 for a U8 is refused in the page; "
         f"SET_EVENT_FILTER sent with enum inputs ({', '.join(options)})")

    # An empty text and one too long are refused with the message of the Python GDS page.
    choose(page, NO_OP_STRING)
    for text in ("", "x" * 60):
        page.fill("[data-arg=arg1]", text)
        sent = len(rows(page, "#command-history"))
        page.click("#command-send")
        assert page.text_content("[data-error-for=arg1]") == "Supply general text of less than 40 characters", text
        assert len(rows(page, "#command-history")) == sent, f"the string {text!r} was sent"
    step("CMD_NO_OP_STRING: an empty text and 60 characters are refused, as in the Python GDS page")
    # A string that contains HTML is shown as text everywhere it appears.
    choose(page, NO_OP_STRING)
    page.fill("[data-arg=arg1]", HTML)
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("CMD_NO_OP_STRING sent")
    history = rows(page, "#command-history")
    assert history[0]["name"] == NO_OP_STRING and history[0]["args"] == f'"{HTML}"', history[0]
    show(page, "events")
    page.wait_for_function("""() => [...document.querySelectorAll('#events-table td[data-col=name]')]
        .some((cell) => cell.textContent.endsWith('NoOpStringReceived'))""")
    text = events_named(page, "NoOpStringReceived")[-1]["text"]
    assert text == f"Received a NO-OP string={HTML}", text
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
    uplink.locator("#files").set_input_files(f"{gds.out}/up.bin")
    uplink.get_by_role("button", name="Add to queue").click()
    expect(uplink.locator("tr", has_text="up.bin")).to_contain_text("finished", timeout=30000)
    expect(uplink.locator("tr", has_text="up.bin")).to_contain_text(f"{len(content)} / {len(content)} bytes")
    received = scenario.wait_for("the file on the Ref side", lambda: os.path.getsize(f"{gds.out}/ref/up.bin") == len(content))
    assert received and open(f"{gds.out}/ref/up.bin", "rb").read() == content
    # Times and destination as the Python GDS page writes them: "Sat, 21 Nov 2026 22:53:23 GMT"; the name alone
    # without a directory, and directory, "/", name with one (`os.path.join` in its uplinker).
    cells = uplink.locator("tr", has_text="up.bin").locator("td").all_text_contents()
    http_date = re.compile(r"^(Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT$")
    assert http_date.match(cells[0]) and http_date.match(cells[1]), cells
    assert parsedate_to_datetime(cells[0]) <= parsedate_to_datetime(cells[1]), cells
    assert cells[4] == "up.bin", cells
    # What the Python page shows and Ref receives for each text of its Destination Folder field
    # (`os.path.join` in its uplinker), read from that page and from `Ref`'s FileReceived event.
    os.makedirs(f"{gds.out}/ref/dir", exist_ok=True)
    joined = [(".", "./"), ("dir", "dir/"), ("dir/", "dir/"), ("dir//", "dir//"), ("./dir", "./dir/"), ("", "")]
    for index, (directory, prefix) in enumerate(joined):
        name = f"up{index + 2}.bin"
        shutil.copy(f"{gds.out}/up.bin", f"{gds.out}/{name}")
        uplink.locator("#files").set_input_files(f"{gds.out}/{name}")
        uplink.locator("#directory").fill(directory)
        uplink.get_by_role("button", name="Add to queue").click()
        expect(uplink.locator("tr", has_text=name)).to_contain_text("finished", timeout=30000)
        assert uplink.locator("tr", has_text=name).locator("td").all_text_contents()[4] == prefix + name, directory
        scenario.wait_for(f"Ref received {prefix}{name}", lambda: any(
            r["body"]["display_text"] == f"Received file {prefix}{name}"
            for r in gds.get("/records?kind=fprime.event&after_seq=0&limit=10000")["records"]))
    uplink.locator("#directory").fill("")
    step(f"Uplink tab: up.bin ({len(content)} bytes) chosen in the page, sent at once and finished; Ref wrote the same bytes; "
         f"times as the Python page ({cells[0]}); destination shown and received by Ref as the Python page for the "
         f"directories {[d for d, _ in joined]}")

    # A file and a data product come down: the Downlink tab lists both, the directory of
    # downlinked files is the watched directory of the data products.
    show(page, "downlink")
    downlink = page.locator("fgds-downlink")
    gds.post("/commands", {"name": "FileHandling.fileDownlink.SendFile", "args": ["down-source.bin", "down.bin"]})
    expect(downlink.locator("tr", has_text="down.bin").first).to_contain_text("finished", timeout=30000)
    expect(downlink.locator("a[download='down.bin']")).to_have_count(1)
    # A part of a file ends as finished with its percentage and no problem counted, as on the Python GDS page.
    gds.post("/commands", {"name": "FileHandling.fileDownlink.SendPartial", "args": ["down-source.bin", "part.bin", 100, 500]})
    expect(downlink.locator("tr", has_text="part.bin").first).to_contain_text("finished", timeout=30000)
    part = downlink.locator("tr", has_text="part.bin").first.locator("td").all_text_contents()
    assert http_date.match(part[0]) and http_date.match(part[1]) and part[5] == "finished" and part[7] == "", part
    assert re.search(r"\(\d+%\)", part[6]) and "(100%)" not in part[6], part
    with page.expect_download() as download:
        downlink.locator("a[download='down.bin']").click()
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
    charts.locator("#chart-add").click()
    charts.locator(".chart-channel").nth(1).select_option("ComCcsds.comQueue.comQueueDepth.1")
    expect(canvas).to_have_count(2)
    scenario.wait_for("a point of the array element", lambda: int(canvas.nth(1).get_attribute("data-points") or 0))
    assert float(canvas.nth(1).get_attribute("data-first")) in [float(r["body"]["value"][1]) for r in depth()[known - 1:]]
    charts.locator(".chart-remove").first.click()
    expect(canvas).to_have_count(1)
    assert charts.locator(".chart-channel").first.input_value() == "ComCcsds.comQueue.comQueueDepth.1"
    step(f"Charts tab: {len(offered)} entries in identifier order, array elements by index; {COUNTER} drawn with the "
         f"vendored Chart.js from the first sample after it was chosen ({len(before)} earlier ones are not drawn), "
         "on an axis that ends at the present; a second chart of an array element")

    show(page, "sequences")
    tab = page.locator("fgds-sequences")
    text = "; two commands\nR00:00:00 CdhCore.cmdDisp.CMD_NO_OP\nR00:00:01 CdhCore.cmdDisp.CMD_NO_OP_STRING \"from a sequence\"\n"
    assert tab.locator("#sequence-text").input_value() == "" and not tab.locator("#sequence-run").is_enabled()
    staged = gds.get("/files/uplink")["directory"]
    tab.locator("#sequence-uplink").click()
    expect(tab.locator("#sequence-error")).to_have_text("Supply filename with .seq suffix")
    tab.locator("#sequence-name").fill("page.seq")
    # A directory of the flight side; `Ref` runs in a directory of this check, and a command takes 40 characters.
    tab.locator("#sequence-directory").fill(".")
    tab.locator("#sequence-text").fill("R00:00:00 Not.a.COMMAND\n")
    tab.locator("#sequence-uplink").click()
    wrong = f"{staged}/page.seq:1: 'Not.a.COMMAND' does not match any command in the command dictionary."
    expect(tab.locator("#sequence-result")).to_have_value(wrong)
    refused = wrong
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
    step(f"Sequences tab: a sequence with an unknown command is refused ({refused.strip()[:60]!r}); one opened from a file is saved, built "
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
