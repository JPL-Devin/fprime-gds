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
    // What the cell shows: an array or a structure is a nested table whose heading is the value.
    const cell = (td) => {
        const nested = td.querySelector("table.embedded_table");
        return (nested ? nested.querySelector("thead th:nth-child(2)") : td).innerText.trim();
    };
    const colour = (row) => [row.querySelector(":scope > td"), row].map((e) => getComputedStyle(e).backgroundColor)
        .find((c) => c !== "rgba(0, 0, 0, 0)") || "";
    return {
        rows: [...table.querySelectorAll(":scope > tbody > tr")].map((row) => [...[...row.querySelectorAll(":scope > td")].map(cell), colour(row)]),
        header: [...table.querySelectorAll(":scope > thead th")].map((th) => th.innerText.trim()).filter((text) => text),
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


# The rows of a table of the new page as it shows them, top to bottom, with the row colour.
NEW_PAGE = """(table) => {
    const seen = (e) => getComputedStyle(e).display !== "none";
    const colour = (row) => [row.querySelector("td"), row].map((e) => getComputedStyle(e).backgroundColor)
        .find((c) => c !== "rgba(0, 0, 0, 0)") || "";
    return {
        header: [...table.querySelectorAll("thead th")].filter(seen).map((th) => th.innerText.trim()),
        rows: [...table.querySelectorAll("tbody tr")].map((row) =>
            [...[...row.querySelectorAll("td")].filter(seen).map((td) => td.innerText.trim()), colour(row)]),
    };
}"""
# Texts the two pages could show differently: an entity, two spaces, quotes, markup.
SPAN, HEADING = '<span title="t">in a span</span><h7>', "<h1>b</h1> after"
TEXTS = [HTML, 'two  spaces &amp; &lt;i&gt; "q" \'s\'', SPAN, HEADING]

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
        for name, arguments in [*scenario.COMMANDS, *(("CdhCore.cmdDisp.CMD_NO_OP_STRING", [text]) for text in TEXTS)]:
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
        # The ground time is a column of the new page only: shown at first, and it can be hidden.
        tables = {}
        for tab in ("events", "channels"):
            show(page, tab)
            assert "Ground time" in page.eval_on_selector(f"#{tab}-table", NEW_PAGE)["header"], f"{tab}: no ground time"
            page.uncheck(f"#{tab}-ground")
            tables[tab] = page.eval_on_selector(f"#{tab}-table", NEW_PAGE)
        page.reload()
        page.wait_for_selector("body[data-ready=true]")
        show(page, "events")
        assert "Ground time" not in page.eval_on_selector("#events-table", NEW_PAGE)["header"], "the hidden column is back after a reload"
        show(page, "channels")
        page.screenshot(path=f"{OUT}/new-channels.png")
        assert not problems, problems
        return tables
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
        tables = {}
        for tab, header in (("events", "Event Severity"), ("channels", "Channel Value")):
            # The tab is shown first: the text of a cell is read as it is rendered.
            page.click(f"li.nav-link:has(#{tab.capitalize()})")
            page.wait_for_timeout(500)
            tables[tab] = {"rows": python_rows(page, header), "header": page.evaluate(PYTHON_PAGE, header)["header"]}
        page.screenshot(path=f"{OUT}/python.png")
        return tables
    finally:
        scenario.stop(gds)


data = capture()
step(f"captured {len(data)} bytes from Ref: {len(scenario.frames(data, FRAMING, FRAME_SIZE))} frames ({FRAMING} framing)")
with sync_playwright() as playwright:
    browser = playwright.chromium.launch()
    new = new_page_tables(browser, data)
    step(f"new page: {len(new['events']['rows'])} events and {len(new['channels']['rows'])} channels shown after the replay")
    old = python_page_tables(browser, data, len(new["events"]["rows"]))
    step(f"Python GDS page: {len(old['events']['rows'])} events and {len(old['channels']['rows'])} channels shown after the replay")
    browser.close()
json.dump({"new": new, "python": old}, open(f"{OUT}/tables.json", "w"), indent=1)

# The same cells, in the same order, with the same row colour: nothing is set aside.
different = []
for tab in ("events", "channels"):
    if [h.lower() for h in new[tab]["header"]] != [h.lower() for h in old[tab]["header"]]:
        different.append(f"{tab} columns: Python GDS page {old[tab]['header']}, new page {new[tab]['header']}")
    ours, theirs = new[tab]["rows"], old[tab]["rows"]
    if tab == "events":
        # README.md: the Python GDS page makes elements of the headings and spans in a text; the new page shows the
        # characters that were sent. The Python text of these two is set aside, the new one is checked below.
        literal = [n for n, row in enumerate(ours) if SPAN in row[4] or HEADING in row[4]]
        assert len(literal) == 2, literal
        for n in literal:
            assert "in a span<h7>" in theirs[n][4] or "b after" in theirs[n][4].replace("\n", " "), theirs[n]
            theirs[n][4] = ours[n][4]
    if len(ours) != len(theirs):
        different.append(f"{tab}: {len(theirs)} rows in the Python GDS page, {len(ours)} in the new page")
    different += [f"{tab} row {n}: Python GDS page {b}, new page {a}" for n, (a, b) in enumerate(zip(ours, theirs)) if a != b]
for line in different[:40]:
    print(f"  DIFFERENT: {line}")
assert not different, f"{len(different)} differences"
events, channels = new["events"]["rows"], new["channels"]["rows"]
texts = [e[4] for e in events]
assert any(HTML in text for text in texts), "the HTML string is not in the events"
assert any('two spaces & <i> "q" \'s\'' in text for text in texts) and any("=" + SPAN in text for text in texts) \
    and any("=" + HEADING in text for text in texts), texts[-4:]
assert all(len(e[0]) == 24 for e in events), "an event time is not to the millisecond"
composite = [c for c in channels if c[3].startswith(("[ ", "{ "))]
assert any(c[3] == '[ "inf", "-inf", "nan" ]' for c in composite), [c[3] for c in composite]
green = [c[2] for c in channels if c[4] == "rgb(195, 230, 203)"]
assert green, "no channel inside its limits is shown green"
step(f"events: the same {len(events)} rows in the same order (time, identifier, name, severity, text, colour) in both pages")
step(f"channels: the same {len(channels)} rows in the same order (last sample time, identifier, name, value, colour) in both pages; "
     f"{len(composite)} arrays and structures, {len(green)} green rows ({', '.join(green)})")
step("the ground time column is hidden by its box and stays hidden after a reload; the other columns are those of the Python GDS page")
print("browser parity: all checks passed")
