#!/usr/bin/env python3
"""The browser UI against a running Ref: what a user sees and does in each tab.

Usage: ui.py <browser-gds binary> <Ref binary> <dictionary> <directory for logs and screenshots>
"""
import os
import sys

from playwright.sync_api import expect, sync_playwright

import harness
from harness import rows, scenario, show, step

BINARY, REF, DICTIONARY, OUT = sys.argv[1:5]
os.makedirs(OUT, exist_ok=True)
TABS = {"commands": "Commanding", "events": "Events", "channels": "Channels", "dictionary": "Dictionaries",
        "logs": "Logs", "advanced": "Advanced"}
ELEMENTS = {"commands": "commands-tab", "events": "events-tab", "channels": "channels-tab",
            "dictionary": "dictionary-tab", "logs": "logs-tab", "advanced": "advanced-tab"}
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
    assert dict(titles) == TABS and [t for t, _ in titles] == list(TABS), titles
    content = {"commands": "#command-select option", "events": "#events-table tbody tr",
               "channels": "#channels-table tbody tr", "dictionary": "#dictionary-table tbody tr",
               "logs": "#logs-select option", "advanced": "#advanced-plugins tbody tr"}
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
    received = events_named(page, "NoOpReceived")[0]
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
    choose(page, "CdhCore.events.SET_EVENT_FILTER")
    options = page.eval_on_selector("select[data-arg=filterLevel]", "(s) => [...s.options].map((o) => o.value)")
    assert "WARNING_HI" in options and "ACTIVITY_LO" in options, options
    page.select_option("select[data-arg=filterLevel]", "ACTIVITY_LO")
    page.select_option("select[data-arg=filterEnabled]", "ENABLED")
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("SET_EVENT_FILTER sent")
    step(f"CMD_TEST_CMD_1 with a text for an I32 and 300 for a U8 is refused in the page; "
         f"SET_EVENT_FILTER sent with enum inputs ({', '.join(options)})")

    # A string that contains HTML is shown as text everywhere it appears.
    choose(page, NO_OP_STRING)
    page.fill("[data-arg=arg1]", HTML)
    page.click("#command-send")
    expect(page.locator("#command-message")).to_contain_text("CMD_NO_OP_STRING sent")
    history = rows(page, "#command-history")
    assert history[0]["name"] == NO_OP_STRING and HTML in history[0]["args"], history[0]
    show(page, "events")
    page.wait_for_function("""() => [...document.querySelectorAll('#events-table td[data-col=name]')]
        .some((cell) => cell.textContent.endsWith('NoOpStringReceived'))""")
    text = events_named(page, "NoOpStringReceived")[0]["text"]
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

    for tab, title in TABS.items():
        show(page, tab)
        page.wait_for_timeout(300)
        page.screenshot(path=f"{OUT}/{title.lower()}.png")
    assert not problems, problems
    step(f"no script error in the page; screenshots in {OUT}")
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
    step("a tab that fails shows an error panel naming its plugin; the six other tabs load and the Events tab stays live")
    page.close()


def run(env, check):
    out = f"{OUT}/{check.__name__}"
    os.makedirs(out, exist_ok=True)
    gds = harness.Gds(BINARY, DICTIONARY, out, env)
    ref = None
    try:
        ref = scenario.start([REF, "-a", "127.0.0.1", "-p", str(gds.link_port)], open(f"{out}/ref.log", "w"), cwd=out)
        scenario.wait_for("telemetry from Ref", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])
        check(browser, gds)
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)


with sync_playwright() as playwright:
    browser = playwright.chromium.launch()
    run({}, stock)
    run({"BROWSER_GDS_BROKEN_TAB": "1"}, broken)
    browser.close()
print("browser ui: all checks passed")
