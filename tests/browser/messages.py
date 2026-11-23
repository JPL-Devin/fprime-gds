"""The messages of the Commanding tab for arguments left empty or holding an invalid text, against those of
the Python GDS page.

    messages.py record PYTHON_GDS_URL FIXTURE    writes the fixture from a running Python GDS page
    messages.py check FGDS DICTIONARY FIXTURE OUT    replays every case of the fixture on the new page

A case is one press of the send button: what is done to each scalar input, in the order of the page, and
what the page shows when the form is drawn (`drawn`), before the press (`before`) and after it: a message
per scalar input, whether the input is marked, the lines under arrays and structs, sent or not. Every case
is played twice on a page opened anew: on the form as the page draws it when the command is chosen
(`chosen`), and after `Clear Arguments` (`cleared`).
"""
import json
import os
import sys

from playwright.sync_api import sync_playwright

# One command per kind of argument of the Ref dictionary: integers of each width and floats (alone and as
# struct members), string, bool, enum, array, struct, struct holding arrays.
COMMANDS = [
    "CdhCore.cmdDisp.CMD_TEST_CMD_1",
    "Ref.typeDemo.SEND_SCALARS",
    "CdhCore.cmdDisp.CMD_NO_OP_STRING",
    "FileHandling.fileManager.RemoveFile",
    "CdhCore.events.SET_EVENT_FILTER",
    "Ref.typeDemo.CHOICES_WITH_FRIENDS",
    "Ref.typeDemo.CHOICE_PAIR_WITH_FRIENDS",
    "Ref.typeDemo.GLUTTON_OF_CHOICE_WITH_FRIENDS",
]
VALID = {"signed": "1", "unsigned": "1", "float": "1.5", "string": "a", "bool": "True"}
# A float input is a number input: of the keys of a text that is not a number it keeps `e` only.
INVALID = {"signed": "abc", "unsigned": "-1", "float": "e", "string": "x" * 300}


def cases(kinds):
    """What is done to each input: None (left as it is), a text to type, or "" (typed, then removed)."""
    typed = [i for i, kind in enumerate(kinds) if kind != "enum"]
    texts = [i for i in typed if kinds[i] != "bool"]
    valid = [VALID.get(kind) for kind in kinds]
    found = [[None] * len(kinds), [INVALID.get(kind) for kind in kinds], valid]
    for i in typed:
        found.append([None if j == i else text for j, text in enumerate(valid)])
    for i in texts:
        found.append([INVALID[kinds[i]] if j == i else text for j, text in enumerate(valid)])
        found.append([INVALID[kinds[i]] if j == i else None for j in range(len(kinds))])
        found.append(["" if j == i else text for j, text in enumerate(valid)])
    return [case for n, case in enumerate(found) if case not in found[:n]]


PYTHON_KINDS = """() => [...document.querySelectorAll(".command-input-form .fprime-scalar-argument")].map((e) => {
    const type = e.__vue__.argument.type;
    if (type.ENUM_DICT) return "enum";
    if (type.name === "BoolType") return "bool";
    if (type.name.indexOf("String") !== -1) return "string";
    if (/^F(32|64)Type$/.test(type.name)) return "float";
    return type.name.startsWith("U") ? "unsigned" : "signed";
})"""
# A message is read when the page displays it.
PYTHON_SHOWN = """() => ({
    scalars: [...document.querySelectorAll(".command-input-form .fprime-scalar-argument")].map((e) => {
        const note = e.querySelector(".invalid-feedback");
        return getComputedStyle(note).display === "none" ? "" : note.textContent.trim();
    }),
    // The mark of the page: its red border, which Bootstrap draws with the place of the message.
    marked: [...document.querySelectorAll(".command-input-form .fprime-scalar-argument")].map((e) =>
        getComputedStyle(e.querySelector(".invalid-feedback")).display !== "none"),
    groups: [...document.querySelectorAll(".command-input-form .fp-error")].map((e) => e.textContent.trim())
        .filter((text) => text),
})"""
NEW_SHOWN = """() => ({
    scalars: [...document.querySelectorAll("#command-form [data-arg]")].map((e) => {
        const note = document.querySelector(`#command-form span[data-error-for="${e.dataset.arg}"]`);
        return note ? note.textContent.trim() : "";
    }),
    marked: [...document.querySelectorAll("#command-form [data-arg]")].map((e) => e.classList.contains("invalid")),
    groups: [...document.querySelectorAll("#command-form div[data-error-for]")].map((e) => e.textContent.trim()),
})"""


def fill(page, field, kind, text):
    if text is None:
        return
    if kind == "bool":
        if field.evaluate("(e) => e.tagName") == "SELECT":
            field.select_option(text.lower())
        else:
            field.locator("input.vs__search").fill(text)
            field.locator("input.vs__search").press("Enter")
        return
    field.fill("")
    field.press_sequentially(text or "1", delay=0)
    if text == "":
        field.press("Backspace")


def press(page, selector, sends):
    """Press the send button; whether the page then asked the GDS to send the command."""
    requests = []
    listener = lambda request: requests.append(request.url) if sends(request) else None
    page.on("request", listener)
    page.click(selector)
    page.wait_for_timeout(250)
    page.remove_listener("request", listener)
    return bool(requests)


STARTS = ["chosen", "cleared"]


def record(url, fixture):
    found = {}
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 2000})
        for command in COMMANDS:
            def choose():
                page.goto(url)
                page.wait_for_selector(".command-input-form")
                page.fill("#mnemonic input.vs__search", command)
                page.press("#mnemonic input.vs__search", "Enter")
                page.wait_for_function("(name) => document.querySelector('.command-input-form').parentElement"
                                       ".querySelector('h2').textContent.endsWith(name)", arg=command)
                page.wait_for_timeout(100)
            choose()
            kinds = page.evaluate(PYTHON_KINDS)
            found[command] = {"kinds": kinds, "cases": []}
            for start in STARTS:
                for case in cases(kinds):
                    choose()
                    if start == "cleared":
                        page.click("button:has-text('Clear Arguments')")
                    drawn = page.evaluate(PYTHON_SHOWN)
                    fields = page.locator(".command-input-form .fprime-scalar-argument")
                    for i, text in enumerate(case):
                        field = fields.nth(i)
                        fill(page, field if kinds[i] == "bool" else field.locator("input.fprime-input"), kinds[i], text)
                    before = page.evaluate(PYTHON_SHOWN)
                    sent = press(page, "button:has-text('Send Command')",
                                 lambda r: r.method == "PUT" and "/commands/" in r.url)
                    found[command]["cases"].append({"start": start, "inputs": case, "drawn": drawn, "before": before,
                                                    **page.evaluate(PYTHON_SHOWN), "sent": sent})
        browser.close()
    with open(fixture, "w") as file:
        json.dump(found, file, indent=1)
        file.write("\n")
    print(f"{sum(len(c['cases']) for c in found.values())} cases of {len(found)} commands written to {fixture}")


def check(binary, dictionary, fixture, out):
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import harness
    os.makedirs(out, exist_ok=True)
    with open(fixture) as file:
        expected = json.load(file)
    assert sorted(expected) == sorted(COMMANDS), "the fixture is not of these commands: record it again"
    gds = harness.Gds(binary, dictionary, out)
    different, count, silent = [], 0, 0
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            problems = []
            page = harness.open_page(browser, gds.url, problems)
            harness.show(page, "commands")
            for command, wanted in expected.items():
                kinds = wanted["kinds"]
                assert [(case["start"], case["inputs"]) for case in wanted["cases"]] == [
                    (start, case) for start in STARTS for case in cases(kinds)], f"{command}: record the fixture again"
                for case in wanted["cases"]:
                    # A page opened anew: every form as it is before anything is typed.
                    page.reload()
                    harness.show(page, "commands")
                    page.select_option("#command-select", command)
                    if case["start"] == "cleared":
                        page.click("#command-clear")
                    drawn = page.evaluate(NEW_SHOWN)
                    fields = page.locator("#command-form [data-arg]")
                    assert fields.count() == len(kinds), f"{command}: {fields.count()} inputs, {len(kinds)} in the Python page"
                    for i, text in enumerate(case["inputs"]):
                        fill(page, fields.nth(i), kinds[i], text)
                    # No Ref is connected: a command the page sends is answered "link down", which is not an error of the page.
                    before = page.evaluate(NEW_SHOWN)
                    sent = press(page, "#command-send", lambda r: r.method == "POST" and r.url.endswith("/commands"))
                    shown = {"drawn": drawn, "before": before, **page.evaluate(NEW_SHOWN), "sent": sent}
                    count += 1
                    if shown != {key: case[key] for key in shown}:
                        different.append((command, case, shown))
                    if not sent:
                        said = any(shown["marked"]) or shown["groups"] or page.locator("#command-message").count()
                        assert said, f"{command} {case['inputs']}: refused with no mark and no word"
                        silent += not (any(shown["marked"]) or shown["groups"])
            browser.close()
    finally:
        gds.stop()
    for command, case, shown in different:
        print(f"  !! {command} {case['inputs']}\n     Python: {json.dumps({k: case[k] for k in shown})}\n     fgds:   {json.dumps(shown)}")
    assert not different, f"{len(different)} of {count} cases differ from the Python GDS page"
    harness.step(f"command messages: {count} submissions of {len(expected)} commands (each input untouched, with an "
                 "invalid text, typed then emptied, alone and with the others invalid; on the form as drawn and after "
                 "Clear Arguments) show the messages and the marks of the Python GDS page when the form is drawn, "
                 f"before Send and after it, and send when it sends; {silent} it refuses with no mark have a line under the form")


if __name__ == "__main__":
    if sys.argv[1] == "record":
        record(*sys.argv[2:])
    else:
        check(*sys.argv[2:])
