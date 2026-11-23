"""Every button, table header, placeholder and label of every tab against the Python GDS page.

    texts.py record PYTHON_GDS_URL FIXTURE              writes the texts of a running Python GDS page
    texts.py check FGDS DICTIONARY FIXTURE DIFFERENCES OUT    reads the same texts from the new page

No Ref is connected: the texts are those of each tab as it opens. A text of one page that the other does
not have must be in DIFFERENCES (per tab and kind, `python` and `fgds`, each text with its reason); a
difference that is not there, or one there that is gone, fails the check.
"""
import json
import os
import sys

from playwright.sync_api import sync_playwright

TABS = ["Commanding", "Events", "Channels", "Uplink", "Downlink", "Dictionaries", "Charts", "Logs", "Sequences", "Advanced"]
KINDS = ["buttons", "headers", "placeholders", "labels"]

# The texts of what is on screen, in the order of the page, each once.
SHOWN = """() => {
    const seen = (e) => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== "hidden";
    const text = (e) => (e.tagName === "INPUT" ? e.value : e.textContent).replace(/\\s+/g, " ").trim();
    // Every element under `root`, through the shadow roots, but the top bar (the tabs and the counters).
    const under = (root) => [...root.querySelectorAll("*")].filter((e) => !e.closest("nav, #tabs, header"))
        .flatMap((e) => (e.shadowRoot ? [e, ...under(e.shadowRoot)] : [e]));
    const elements = under(document).filter(seen);
    const all = (selector, read) => [...new Set(elements.filter((e) => e.matches(selector)).map(read).filter((t) => t))];
    return {
        buttons: all("button, input[type=button], input[type=submit], a.btn", (e) => text(e) || e.title || e.getAttribute("aria-label") || ""),
        headers: all("th", text),
        placeholders: all("input[placeholder], textarea[placeholder]", (e) => e.placeholder.trim()),
        // The text of a label without that of a list or a field inside it.
        labels: all("label, h1, h2, h3, h4, h5, legend, .input-group-text", (e) => {
            const copy = e.cloneNode(true);
            copy.querySelectorAll("select, input, textarea, button").forEach((inner) => inner.remove());
            return copy.textContent.replace(/\\s+/g, " ").trim();
        }),
    };
}"""


def record(url, fixture):
    found = {}
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 2000})
        page.goto(url)
        page.wait_for_selector(".command-input-form")
        for tab in TABS:
            page.click(f"li.nav-link:has(#{tab})")
            page.wait_for_timeout(500)
            found[tab] = page.evaluate(SHOWN)
        browser.close()
    with open(fixture, "w") as file:
        json.dump(found, file, indent=1)
        file.write("\n")
    print(f"{sum(len(texts) for tab in found.values() for texts in tab.values())} texts written to {fixture}")


def check(binary, dictionary, fixture, differences, out):
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import harness
    os.makedirs(out, exist_ok=True)
    with open(fixture) as file:
        expected = json.load(file)
    with open(differences) as file:
        listed = json.load(file)
    gds = harness.Gds(binary, dictionary, out)
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            page = harness.open_page(browser, gds.url, [])
            names = dict(page.eval_on_selector_all("#tabs button", "all => all.map((b) => [b.textContent.trim(), b.dataset.tab])"))
            shown = {}
            for tab in TABS:
                harness.show(page, names[tab])
                page.wait_for_timeout(300)
                shown[tab] = page.evaluate(SHOWN)
            browser.close()
    finally:
        gds.stop()
    with open(f"{out}/fgds-page.json", "w") as file:
        json.dump(shown, file, indent=1)
    wrong, equal, different = [], 0, 0
    for tab in TABS:
        for kind in KINDS:
            old, new = expected[tab][kind], shown[tab][kind]
            known = listed.get(tab, {}).get(kind, {})
            for side, only in (("python", [t for t in old if t not in new]), ("fgds", [t for t in new if t not in old])):
                reasons = known.get(side, {})
                wrong += [f"{tab} {kind}: {text!r} only on the {side} page, not listed" for text in only if text not in reasons]
                wrong += [f"{tab} {kind}: {text!r} listed as only on the {side} page, and it is not" for text in reasons if text not in only]
                different += len(only)
            equal += sum(text in new for text in old)
    for line in wrong:
        print(f"  !! {line}")
    with open(f"{out}/differences.json", "w") as file:
        json.dump({tab: {kind: {"python": [t for t in expected[tab][kind] if t not in shown[tab][kind]],
                                "fgds": [t for t in shown[tab][kind] if t not in expected[tab][kind]]}
                         for kind in KINDS} for tab in TABS}, file, indent=1)
    assert not wrong, f"{len(wrong)} texts differ from the Python GDS page and the list of differences"
    harness.step(f"texts of the page: buttons, table headers, placeholders and labels of the {len(TABS)} tabs of the Python GDS "
                 f"page; {equal} are on both pages, {different} are on one page only, each listed with its reason")


if __name__ == "__main__":
    if sys.argv[1] == "record":
        record(*sys.argv[2:])
    else:
        check(*sys.argv[2:])
