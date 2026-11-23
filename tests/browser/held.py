"""Lines typed one after the other in the `Command String` input, against the Python GDS page.

    held.py record PYTHON_GDS_URL FIXTURE    types the sequences of the fixture again in a running Python GDS page
    held.py check FGDS DICTIONARY FIXTURE OUT    types them in the new page

A sequence starts from a page just loaded. A line that gives part of an array or of a structure keeps what the
form of that command already holds; each step holds what `line.py` records for one line.
"""
import json
import os
import sys

from playwright.sync_api import sync_playwright

import harness
import line


def record(url, fixture, cases):
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 2000})
        for case in cases:
            page.goto(url)
            page.wait_for_selector(".command-input-form")
            for step in case:
                page.fill("input[name=command-text]", step["typed"])
                page.press("input[name=command-text]", "Tab")
                page.wait_for_timeout(300)
                step.update(page.evaluate(line.PYTHON_SHOWN))
        browser.close()
    with open(fixture, "w") as file:
        json.dump(cases, file, indent=1)
        file.write("\n")
    print(f"{len(cases)} sequences written to {fixture}")


def check(binary, dictionary, cases, out):
    os.makedirs(out, exist_ok=True)
    gds = harness.Gds(binary, dictionary, out)
    different = []
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            page = harness.open_page(browser, gds.url, [])
            for case in cases:
                page.reload()
                page.wait_for_selector("body[data-ready=true]")
                harness.show(page, "commands")
                for step in case:
                    page.fill("#command-string", step["typed"])
                    page.press("#command-string", "Tab")
                    page.wait_for_timeout(100)
                    shown = page.evaluate(line.NEW_SHOWN)
                    wanted = {key: step[key] for key in shown}
                    wanted["inputs"], shown["inputs"] = line.plain(wanted["inputs"]), line.plain(shown["inputs"])
                    if shown != wanted:
                        different.append(([s["typed"] for s in case], step["typed"], wanted, shown))
            browser.close()
    finally:
        gds.stop()
    for typed, at, wanted, shown in different:
        print(f"DIFFERENT {typed} at {at!r}:")
        for key in shown:
            if shown[key] != wanted[key]:
                print(f"  {key}: Python GDS page {wanted[key]!r}, new page {shown[key]!r}")
    if different:
        sys.exit(f"{len(different)} sequences differ from the Python GDS page")
    print(f"  ok: {len(cases)} sequences of Command String lines: form, line and messages as the Python GDS page at each step")


if __name__ == "__main__":
    with open(sys.argv[3] if sys.argv[1] == "record" else sys.argv[4]) as file:
        held = json.load(file)
    if sys.argv[1] == "record":
        record(sys.argv[2], sys.argv[3], held)
    else:
        check(sys.argv[2], sys.argv[3], held, sys.argv[5])
