#!/usr/bin/env python3
"""The Events and Channels tables of the Python GDS page and of the new page, for the same
bytes: a Ref session is captured once, then replayed into each GDS while its page is open.

Usage: parity.py <fgds binary> <Ref binary> <dictionary> <fprime-gds executable> <directory for logs> [ccsds|fprime]

The accepted differences are listed in README.md.
"""
import json
import os
import socket
import sys
import time
import urllib.request

from playwright.sync_api import sync_playwright

import harness
from harness import rows, scenario, show, step

BINARY, REF, DICTIONARY, PYTHON_GDS, OUT = sys.argv[1:6]
FRAMING = sys.argv[6] if len(sys.argv) > 6 else "ccsds"  # the framing Ref was built with
os.makedirs(OUT, exist_ok=True)
HTML = "<b>bold</b><img src=x onerror=x=1>"

# The rows of the table of the Python GDS page that has this column, as the page shows them.
PYTHON_PAGE = """(header) => {
    const table = [...document.querySelectorAll("table")].find((t) =>
        [...t.querySelectorAll(":scope > thead th")].some((th) => th.textContent.trim() === header));
    const view = table.closest(".fp-flex-repeater");
    const button = (text) => [...view.querySelectorAll("button")].find((b) => b.textContent.trim() === text);
    const cell = (td) => {
        const nested = td.querySelector("table.embedded_table");
        return nested ? {json: nested.querySelector("thead th:nth-child(2)").textContent} : td.textContent.trim();
    };
    return {
        rows: [...table.querySelectorAll(":scope > tbody > tr")].map((row) => [...row.querySelectorAll(":scope > td")].map(cell)),
        last: button(">").classList.contains("disabled"),
        // "a - b": the positions of the rows shown; ">" moves by less than a page.
        range: [...view.querySelectorAll("*")].map((e) => e.children.length ? "" : e.textContent.trim())
            .find((text) => /^[0-9]+ - [0-9]+$/.test(text)).split(" - ").map(Number),
    };
}"""
PYTHON_CLICK = """([header, text]) => {
    const table = [...document.querySelectorAll("table")].find((t) =>
        [...t.querySelectorAll(":scope > thead th")].some((th) => th.textContent.trim() === header));
    [...table.closest(".fp-flex-repeater").querySelectorAll("button")].find((b) => b.textContent.trim() === text).click();
}"""


def one_line(value):
    """A decoded value as the new page writes it: [a, b] and {name: value}."""
    if isinstance(value, list):
        return "[" + ", ".join(one_line(v) for v in value) + "]"
    if isinstance(value, dict):
        return "{" + ", ".join(f"{k}: {one_line(v)}" for k, v in value.items()) + "}"
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)

FRAME_SIZE = scenario.tm_frame_size(DICTIONARY)


def capture():
    """A Ref session with the commands of the comparison and a string that contains HTML."""
    out = f"{OUT}/capture"
    os.makedirs(out, exist_ok=True)
    gds = harness.Gds(BINARY, DICTIONARY, out, framing=FRAMING)
    ref = None
    try:
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(REF, proxy, out)
        scenario.wait_for("telemetry", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])
        for name, arguments in [*scenario.COMMANDS, ("CdhCore.cmdDisp.CMD_NO_OP_STRING", [HTML])]:
            gds.post("/commands", {"name": name, "args": arguments})
            time.sleep(scenario.GAP)
        time.sleep(scenario.SETTLE)
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)
    # Whole frames only: the capture ends wherever Ref was stopped.
    data = scenario.whole_frames(bytes(proxy.downlink), FRAMING, FRAME_SIZE)
    open(f"{OUT}/downlink.bin", "wb").write(data)
    return data


def new_page_tables(browser, data):
    out = f"{OUT}/rust"
    os.makedirs(out, exist_ok=True)
    gds = harness.Gds(BINARY, DICTIONARY, out, framing=FRAMING)
    try:
        problems = []
        page = harness.open_page(browser, gds.url, problems)
        show(page, "channels")
        show(page, "events")
        flight = scenario.wait_for("the link", lambda: socket.create_connection(("127.0.0.1", gds.link_port)))
        # One half arrives while the page is open; the page is then reloaded, so that half
        # comes from the history and the other half from the stream.
        half = len(data) // 2 // FRAME_SIZE * FRAME_SIZE if FRAMING == "ccsds" else len(data) // 2
        flight.sendall(data[:half])
        page.wait_for_selector("#events-table tbody tr")
        page.reload()
        page.wait_for_selector("body[data-ready=true]")
        show(page, "channels")
        show(page, "events")
        flight.sendall(data[half:])
        scenario.steady(lambda: gds.get("/status")["counters"].get("packets_received", 0))
        count = len(gds.get("/records?kind=fprime.event&limit=10000")["records"])
        page.wait_for_function(f"() => document.querySelectorAll('#events-table tbody tr').length === {count}")
        newest = max(int(r["seq"]) for r in gds.get("/records/latest?kind=fprime.channel")["records"])
        show(page, "channels")
        page.wait_for_function(f"""() => Math.max(...[...document.querySelectorAll('#channels-table tbody tr')]
            .map((row) => Number(row.dataset.seq))) === {newest}""")
        events = [(r["time"], r["id"], r["name"], r["severity"], r["text"]) for r in rows(page, "#events-table")]
        channels = {r["name"]: (r["time"], r["id"], r["value"]) for r in rows(page, "#channels-table")}
        page.screenshot(path=f"{OUT}/new-channels.png")
        assert not problems, problems
        return events, channels
    finally:
        gds.stop()


def python_rows(page, header):
    """Every row of a table of the Python GDS page: it shows one page of rows at a time."""
    page.evaluate(PYTHON_CLICK, [header, "First"])
    found, before = [], None
    while True:
        page.wait_for_timeout(300)
        shown = page.evaluate(PYTHON_PAGE, header)
        first, end = shown["range"]
        if shown["range"] == before:  # ">" moved nothing: the end of the table
            return found
        assert first <= len(found) and end - first == len(shown["rows"]), f"rows {first} to {end} after {len(found)} rows"
        found += shown["rows"][len(found) - first:]
        if shown["last"]:
            return found
        before = shown["range"]
        page.evaluate(PYTHON_CLICK, [header, ">"])


def python_page_tables(browser, data, events_wanted):
    out = f"{OUT}/python"
    os.makedirs(out, exist_ok=True)
    http, link, tts = scenario.free_port(), scenario.free_port(), scenario.free_port()
    gds = scenario.start(
        [PYTHON_GDS, "-n", "--dictionary", DICTIONARY, "-g", "html", "--gui-addr", "127.0.0.1", "--gui-port", str(http),
         "--ip-address", "127.0.0.1", "--ip-port", str(link), "--tts-addr", "127.0.0.1", "--tts-port", str(tts),
         "--no-zmq", "--framing-selection", "fprime" if FRAMING == "fprime" else "space-packet-space-data-link", "--logs", f"{out}/logs", "--file-storage-directory", f"{out}/files"],
        open(f"{out}/gds.log", "w"), env={**os.environ, "BROWSER": "true"}, cwd=out,
    )
    try:
        flight = scenario.wait_for("the Python GDS link port", lambda: socket.create_connection(("127.0.0.1", link)))
        scenario.wait_for("the Python GDS page", lambda: urllib.request.urlopen(f"http://127.0.0.1:{http}/").read())
        page = browser.new_page(viewport={"width": 1400, "height": 850})
        # A session of the Python GDS sees what arrives after its first request: wait for it.
        polled = set()
        page.on("response", lambda r: [polled.add(k) for k in ("/events?session", "/channels?session") if k in r.url])
        page.goto(f"http://127.0.0.1:{http}/")
        page.wait_for_selector("text=Event Severity", state="attached")
        scenario.wait_for("the first requests of the Python GDS page", lambda: page.wait_for_timeout(200) or len(polled) == 2)
        flight.sendall(data)
        end, events = time.monotonic() + 60, []
        while len(events) < events_wanted and time.monotonic() < end:
            page.wait_for_timeout(1000)
            events = python_rows(page, "Event Severity")
        page.wait_for_timeout(2000)  # the page polls: let the last channel samples arrive
        events = [tuple(cells) for cells in python_rows(page, "Event Severity")]
        channels = {}
        for sample_time, identifier, name, value in python_rows(page, "Channel Value"):
            channels[name] = (sample_time, identifier, one_line(json.loads(value["json"])) if isinstance(value, dict) else value)
        page.screenshot(path=f"{OUT}/python.png")
        return events, channels
    finally:
        scenario.stop(gds)


def milliseconds(row):
    """Accepted difference 1 of README.md: the Python GDS page shows times to the millisecond."""
    return (row[0][:23] + "Z", *row[1:])


data = capture()
step(f"captured {len(data)} bytes from Ref: {len(scenario.frames(data, FRAMING, FRAME_SIZE))} frames ({FRAMING} framing)")
with sync_playwright() as playwright:
    browser = playwright.chromium.launch()
    new_events, new_channels = new_page_tables(browser, data)
    step(f"new page: {len(new_events)} events and {len(new_channels)} channels shown after the replay")
    old_events, old_channels = python_page_tables(browser, data, len(new_events))
    step(f"Python GDS page: {len(old_events)} events and {len(old_channels)} channels shown after the replay")
    browser.close()
json.dump({"new": {"events": new_events, "channels": new_channels}, "python": {"events": old_events, "channels": old_channels}},
          open(f"{OUT}/tables.json", "w"), indent=1)
assert all(len(e[0]) == 27 for e in new_events), "the new page does not show microseconds"
new_events = [milliseconds(e) for e in new_events]
new_channels = {name: milliseconds(row) for name, row in new_channels.items()}

different = [f"event only in the Python GDS page: {e}" for e in old_events if e not in new_events]
different += [f"event only in the new page: {e}" for e in new_events if e not in old_events]
if sorted(old_events) != sorted(new_events) and not different:
    different.append("the same events, a different number of times")
for name in sorted(set(old_channels) | set(new_channels)):
    if old_channels.get(name) != new_channels.get(name):
        different.append(f"channel {name}: Python GDS page {old_channels.get(name)}, new page {new_channels.get(name)}")
for line in different[:40]:
    print(f"  DIFFERENT: {line}")
assert not different, f"{len(different)} differences"
assert any(HTML in e[4] for e in new_events), "the HTML string is not in the events"
step(f"events: the same {len(new_events)} rows (time, identifier, name, severity, text) in both pages")
step(f"channels: the same {len(new_channels)} rows (name, identifier, last sample time, value) in both pages")
print("browser parity: all checks passed")
