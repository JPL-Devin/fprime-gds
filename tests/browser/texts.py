"""The texts of the page against those of the Python GDS page: buttons, table headers, placeholders, labels,
hover texts (`title`, `alt`, `aria-label`), every other text on screen, and the title of the window.

    texts.py record PYTHON_GDS REF DICTIONARY FIXTURE OUT                 starts a Python GDS and reads its page
    texts.py check FGDS REF DICTIONARY FIXTURE DIFFERENCES DEVIATIONS OUT    reads the same from the new page

Both pages are taken through the same states (`walk`): each tab as it opens with no Ref, the top bar, the form
of each command of COMMANDS, a command sent with no Ref, Charts with its help, a chart added and a channel
chosen, then with Ref running each tab again, the top bar and the chart. The rows of the tables are data, compared
by parity.py, and are left out, except the row of the command sent with no Ref.

A text of one page that the other does not have must be in DIFFERENCES (per state and kind, `python` and
`fgds`, each text with the row of DEVIATIONS that lists it, as `T<number>`); a difference that is not there, one
there that is gone, or one whose row DEVIATIONS does not have, fails the check.
"""
import json
import os
import re
import socket
import sys
import urllib.request

from playwright.sync_api import sync_playwright

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402
import scenario  # noqa: E402

TABS = ["Commanding", "Events", "Channels", "Uplink", "Downlink", "Dictionaries", "Charts", "Logs", "Sequences", "Advanced"]
KINDS = ["buttons", "headers", "placeholders", "labels", "titles", "texts", "title"]
NO_OP = "CdhCore.cmdDisp.CMD_NO_OP"
# A form of every kind of argument: numbers, text, Boolean, enumeration, structure, array, and the nested ones.
COMMANDS = ["CdhCore.cmdDisp.CMD_TEST_CMD_1", "Ref.typeDemo.SEND_SCALARS", "CdhCore.cmdDisp.CMD_NO_OP_STRING",
            "FileHandling.fileManager.RemoveFile", "CdhCore.events.SET_EVENT_FILTER", "Ref.typeDemo.CHOICES_WITH_FRIENDS",
            "Ref.typeDemo.CHOICE_PAIR_WITH_FRIENDS", "Ref.typeDemo.GLUTTON_OF_CHOICE_WITH_FRIENDS"]
CHANNEL = "CdhCore.cmdDisp.CommandsDispatched"

# The texts of what is on screen, in the order of the page, each once. `bar`: the top bar only, or all but it.
# `rows`: with the rows of the tables.
SHOWN = """({bar, rows}) => {
    const seen = (e) => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== "hidden";
    const clean = (t) => t.replace(/\\s+/g, " ").trim();
    const text = (e) => clean(e.tagName === "INPUT" ? e.value : e.textContent);
    const under = (root) => [...root.querySelectorAll("*")].flatMap((e) => (e.shadowRoot ? [e, ...under(e.shadowRoot)] : [e]));
    const top = "nav, #tabs, header";
    const elements = under(document).filter((e) => Boolean(e.closest(top)) === bar && (rows || !e.closest("tbody")) && seen(e));
    const all = (selector, read) => [...new Set(elements.filter((e) => e.matches(selector)).map(read).filter((t) => t))];
    const button = "button, input[type=button], input[type=submit], a.btn";
    const label = "label, h1, h2, h3, h4, h5, legend, .input-group-text";
    // The lists of choices, and what is typed or drawn, are not texts of the page.
    const other = `${button}, ${label}, th, option, select, textarea, pre, script, style, canvas, .vs__dropdown-menu, .vs__selected, .CodeMirror`;
    return {
        // In the top bar a tab or a counter is a button on one page and not on the other: all are texts there.
        buttons: bar ? [] : all(button, (e) => text(e) || e.title || e.getAttribute("aria-label") || ""),
        headers: all("th", text),
        // The hover text of anything, and the text that stands for an image.
        // That of an entry of a list or of a table cell is data: its kind stands for it.
        titles: [...new Set(under(document).filter((e) => Boolean(e.closest(top)) === bar && seen(e.closest("option") ? e.closest("select") : e)
            && e.matches("[title], [alt], [aria-label]")).map((e) => (e.matches("option") ? "<an entry of a list>"
            : e.closest("tbody") ? (e.matches("td") ? "<a table cell>" : "<inside a table cell>")
            : clean(e.title || e.getAttribute("alt") || e.getAttribute("aria-label") || ""))).filter((t) => t))],
        // What a field shows while it is empty; for a list, its entry with no value.
        placeholders: all("input[placeholder], textarea[placeholder], select", (e) => (e.tagName !== "SELECT" ? e.placeholder.trim()
            : clean(e.selectedOptions.length && e.selectedOptions[0].value === "" ? e.selectedOptions[0].textContent : ""))),
        // The text of a label without that of a list or a field inside it.
        labels: all(label, (e) => {
            const copy = e.cloneNode(true);
            copy.querySelectorAll("select, input, textarea, button").forEach((inner) => inner.remove());
            return clean(copy.textContent);
        }),
        // Every other piece of text.
        texts: [...new Set(elements.filter((e) => !e.closest(bar ? "script, style" : other)).flatMap((e) => [...e.childNodes])
            .filter((n) => n.nodeType === Node.TEXT_NODE).map((n) => clean(n.textContent)).filter((t) => t))],
        title: bar && document.title ? [document.title] : [],
    };
}"""


def normal(shown, dictionary, live):
    """Without what changes from run to run: the versions of the dictionary, times, counts."""
    with open(dictionary) as file:
        metadata = json.load(file)["metadata"]

    def one(kind, text):
        for key in ("projectVersion", "frameworkVersion"):
            text = text.replace(metadata[key], "<version>")
        if kind == "texts" or (live and kind == "labels"):
            text = re.sub(r"\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d(\.\d+)?|\d\d:\d\d:\d\d(\.\d+)?", "<time>", text)
            text = re.sub(r"\d+(\.\d+)?", "N", text)
        return text
    return {kind: list(dict.fromkeys(one(kind, text) for text in texts)) for kind, texts in shown.items()}


class Python:
    """The page of a Python GDS started here."""

    def __init__(self, browser, executable, ref, dictionary, out):
        self.ref, self.out = ref, out
        http, self.link, tts = scenario.free_port(), scenario.free_port(), scenario.free_port()
        self.gds = scenario.start(
            [executable, "-n", "--dictionary", dictionary, "-g", "html", "--gui-addr", "127.0.0.1", "--gui-port", str(http),
             "--ip-address", "127.0.0.1", "--ip-port", str(self.link), "--tts-addr", "127.0.0.1", "--tts-port", str(tts),
             "--no-zmq", "--framing-selection", "space-packet-space-data-link", "--logs", f"{out}/logs",
             "--file-storage-directory", f"{out}/files"],
            open(f"{out}/gds.log", "w"), env={**os.environ, "BROWSER": "true"}, cwd=out)
        scenario.wait_for("the Python GDS link port", lambda: socket.create_connection(("127.0.0.1", self.link))).close()
        scenario.wait_for("the Python GDS page", lambda: urllib.request.urlopen(f"http://127.0.0.1:{http}/").read())
        self.page = browser.new_page(viewport={"width": 1400, "height": 2000})
        self.page.goto(f"http://127.0.0.1:{http}/")
        self.page.wait_for_selector(".command-input-form")

    def tab(self, name):
        self.page.click(f"li.nav-link:has(#{name})")

    def command(self, name):
        self.page.fill("#mnemonic input.vs__search", name)
        self.page.press("#mnemonic input.vs__search", "Enter")

    def send(self):
        self.page.click("button:has-text('Send Command')")

    def chart(self, step):
        if step == "channel":
            self.page.fill("#channelList input.vs__search", CHANNEL)
            self.page.press("#channelList input.vs__search", "Enter")
        else:
            self.page.locator(f"button:has-text('{'Help' if step == 'help' else 'Add Chart'}'):visible").first.click()

    def start_ref(self):
        os.makedirs(f"{self.out}/ref", exist_ok=True)
        self.flight = scenario.start([self.ref, "-a", "127.0.0.1", "-p", str(self.link)], open(f"{self.out}/ref.log", "w"), cwd=f"{self.out}/ref")
        self.tab("Events")
        self.page.wait_for_selector("tbody tr:visible", timeout=60000)

    def stop(self):
        if hasattr(self, "flight"):
            scenario.stop(self.flight)
        scenario.stop(self.gds)


class New:
    """The page of the new GDS."""

    def __init__(self, browser, binary, ref, dictionary, out):
        self.ref, self.out = ref, out
        self.gds = harness.Gds(binary, dictionary, out)
        self.page = harness.open_page(browser, self.gds.url, [])
        self.page.set_viewport_size({"width": 1400, "height": 2000})
        self.names = dict(self.page.eval_on_selector_all("#tabs button", "all => all.map((b) => [b.textContent.trim(), b.dataset.tab])"))

    def tab(self, name):
        harness.show(self.page, self.names[name])

    def command(self, name):
        self.page.select_option("#command-select", name)

    def send(self):
        self.page.click("#command-send")

    def chart(self, step):
        if step == "channel":
            self.page.locator(".chart-channel").first.select_option(CHANNEL)
        else:
            self.page.click("#chart-help" if step == "help" else "#chart-add")

    def start_ref(self):
        os.makedirs(f"{self.out}/ref", exist_ok=True)
        self.flight = scenario.start([self.ref, "-a", "127.0.0.1", "-p", str(self.gds.link_port)], open(f"{self.out}/ref.log", "w"), cwd=f"{self.out}/ref")
        self.tab("Events")
        self.page.wait_for_selector("#tab tbody tr:visible", timeout=60000)

    def stop(self):
        if hasattr(self, "flight"):
            scenario.stop(self.flight)
        self.gds.stop()


def walk(side, dictionary):
    """The states of the page, by name, each with its texts."""
    found = {}

    def take(state, bar=False, rows=False, live=False):
        side.page.wait_for_timeout(500)
        found[state] = normal(side.page.evaluate(SHOWN, {"bar": bar, "rows": rows}), dictionary, live)

    for tab in TABS:
        side.tab(tab)
        take(tab)
    take("Top bar", bar=True)
    side.tab("Commanding")
    for command in COMMANDS:
        side.command(command)
        take(f"Commanding, {command}")
    side.command(NO_OP)
    side.send()
    take("Commanding, sent with no Ref", rows=True)
    side.tab("Charts")
    for step in ("help", "add", "channel"):
        side.chart(step)
        take(f"Charts, {step}")
    side.start_ref()
    side.page.wait_for_timeout(5000)
    for tab in TABS:
        side.tab(tab)
        take(f"{tab}, Ref running", live=True)
    take("Top bar, Ref running", bar=True, live=True)
    return found


def record(executable, ref, dictionary, fixture, out):
    os.makedirs(out, exist_ok=True)
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        side = Python(browser, executable, ref, dictionary, out)
        try:
            found = walk(side, dictionary)
        finally:
            side.stop()
        browser.close()
    with open(fixture, "w") as file:
        json.dump(found, file, indent=1, ensure_ascii=False)
        file.write("\n")
    print(f"{sum(len(texts) for state in found.values() for texts in state.values())} texts of {len(found)} states written to {fixture}")


def check(binary, ref, dictionary, fixture, differences, deviations, out):
    os.makedirs(out, exist_ok=True)
    with open(fixture) as file:
        expected = json.load(file)
    with open(differences) as file:
        listed = json.load(file)
    with open(deviations) as file:
        rows = set(re.findall(r"^\| (T\d+) \|", file.read(), re.MULTILINE))
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        side = New(browser, binary, ref, dictionary, out)
        try:
            shown = walk(side, dictionary)
        finally:
            side.stop()
        browser.close()
    with open(f"{out}/fgds-page.json", "w") as file:
        json.dump(shown, file, indent=1, ensure_ascii=False)
    assert list(shown) == list(expected), "record the fixture again: the states are not those of the check"
    wrong, equal, different, used = [], 0, 0, set()
    for state in expected:
        for kind in KINDS:
            old, new = expected[state][kind], shown[state][kind]
            known = listed.get(state, {}).get(kind, {})
            for side, only in (("python", [t for t in old if t not in new]), ("fgds", [t for t in new if t not in old])):
                rows_of = known.get(side, {})
                wrong += [f"{state}, {kind}: {text!r} only on the {side} page, not listed" for text in only if text not in rows_of]
                wrong += [f"{state}, {kind}: {text!r} listed as only on the {side} page, and it is not" for text in rows_of if text not in only]
                wrong += [f"{state}, {kind}: {text!r} is listed under {row}, which {deviations} does not have"
                          for text, row in rows_of.items() if row not in rows]
                used |= set(rows_of.values())
                different += len(only)
            equal += sum(text in new for text in old)
    wrong += [f"row {row} of {deviations} has no text in {differences}" for row in sorted(rows - used)]
    for line in wrong:
        print(f"  !! {line}")
    with open(f"{out}/differences.json", "w") as file:
        json.dump({state: {kind: {"python": [t for t in expected[state][kind] if t not in shown[state][kind]],
                                  "fgds": [t for t in shown[state][kind] if t not in expected[state][kind]]}
                           for kind in KINDS} for state in expected}, file, indent=1, ensure_ascii=False)
    assert not wrong, f"{len(wrong)} texts differ from the Python GDS page and the list of differences"
    harness.step(f"texts of the page: buttons, table headers, placeholders, labels, hover texts, other texts and the window title in "
                 f"{len(expected)} states of the Python GDS page (tabs, top bar, command forms, charts; without and with Ref); "
                 f"{equal} are on both pages, {different} are on one page only, each in a row of deviations.md")


if __name__ == "__main__":
    if sys.argv[1] == "record":
        record(*sys.argv[2:])
    else:
        check(*sys.argv[2:])
