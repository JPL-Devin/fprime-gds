"""The `Command String` input of the Commanding tab against that of the Python GDS page.

    line.py record PYTHON_GDS_URL FIXTURE    writes the fixture from a running Python GDS page
    line.py check FGDS DICTIONARY FIXTURE OUT    types every line of the fixture in the new page

A case is one line typed in the input: what each scalar input of the form then holds, the line the input
then shows, the message under it, and, when the command is sent, the arguments the page gives the GDS.
"""
import json
import os
import sys

from playwright.sync_api import sync_playwright

LINES = [
    "CdhCore.cmdDisp.CMD_NO_OP",
    'CdhCore.cmdDisp.CMD_NO_OP_STRING, "hello world"',
    'CdhCore.cmdDisp.CMD_NO_OP_STRING, "a, b: [c] {d}"',
    'CdhCore.cmdDisp.CMD_NO_OP_STRING, "say \\"hi\\""',
    "CdhCore.cmdDisp.CMD_NO_OP_STRING, bare_word",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1, 2.5, 3",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, -7, 1e3, 255",
    "cdhcore.cmddisp.cmd_test_cmd_1, 4, -0.25, 6",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1",
    "CdhCore.events.SET_EVENT_FILTER, WARNING_HI, ENABLED",
    'FileHandling.fileManager.RemoveFile, "/tmp/a file.txt", true',
    'FileHandling.fileManager.MoveFile, "/tmp/a", "/tmp/b"',
    "Ref.typeDemo.CHOICE, RED",
    "Ref.typeDemo.CHOICES, [RED, BLUE]",
    "Ref.typeDemo.CHOICE_PAIR, {firstChoice: RED, secondChoice: BLUE}",
    "Ref.typeDemo.SEND_SCALARS, {i8: -1, i16: 2, i32: -3, i64: 4, u8: 5, u16: 6, u32: 7, u64: 18446744073709551615, f32: 1.5, f64: -2.25}",
    "Ref.typeDemo.DUMP_FLOATS",
    "Ref.typeDemo.CHOICES_PRM_PRM_SET, [BLUE, RED]",
    "No.such.COMMAND, 1",
    "CdhCore.cmdDisp.CMD_NO_OP_STRING, \"unfinished",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1, , 3",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1, 2.5, 3, 4",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1, 1.5, 0x1F",
    "CdhCore.cmdDisp.CMD_NO_OP",
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 9",  # too few arguments for a command the form does not show
    "CdhCore.version.VERSION, NOT_A_CONSTANT",
    "NO_SUCH.COMMAND, 1",
]

PYTHON_SHOWN = """() => ({
    name: (document.querySelector('.command-input-form').parentElement.querySelector('h2') || {textContent: ""}).textContent.trim().split(" ").pop(),
    inputs: [...document.querySelectorAll(".command-input-form .fprime-scalar-argument")].map((e) => {
        const value = e.__vue__.argument.value;
        return value === null || value === undefined ? "" : String(value);
    }),
    line: document.querySelector("input[name=command-text]").value,
    error: (() => {
        const input = document.querySelector("input[name=command-text]");
        return input.classList.contains("is-invalid") ? input.parentElement.querySelector(".invalid-feedback").textContent.trim() : "";
    })(),
    // The messages under the mnemonic and under the arguments.
    messages: [...document.querySelectorAll(".command-input-form .invalid-feedback, .command-input-form .text-danger")]
        .filter((e) => e.getClientRects().length).map((e) => e.textContent.trim()).filter((text) => text),
})"""
NEW_SHOWN = """() => ({
    name: (document.querySelector("#command-name") || {textContent: ""}).textContent.trim().split(" ")[0],
    inputs: [...document.querySelectorAll("#command-form [data-arg]")].map((e) => e.value),
    line: document.querySelector("#command-string").value,
    error: (document.querySelector("#command-string-error") || {textContent: ""}).textContent.trim(),
    messages: [...document.querySelectorAll("#command-form .error-text")].map((e) => e.textContent.trim()).filter((text) => text),
})"""


def plain(inputs):
    """The texts of the inputs, a bool as the new page names it (the Python page writes True and False)."""
    return [{"True": "true", "False": "false"}.get(text, text) for text in inputs]


def record(url, fixture):
    found = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 2000})
        page.goto(url)
        page.wait_for_selector(".command-input-form")
        for line in LINES:
            page.fill("input[name=command-text]", line)
            page.press("input[name=command-text]", "Tab")
            page.wait_for_timeout(300)
            found.append({"typed": line, **page.evaluate(PYTHON_SHOWN)})
        browser.close()
    with open(fixture, "w") as file:
        json.dump(found, file, indent=1)
        file.write("\n")
    print(f"{len(found)} lines written to {fixture}")


# Lines with more arguments than the command takes, and the refusal of fgds.
EXCESS = {
    "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1, 2.5, 3, 4": "CdhCore.cmdDisp.CMD_TEST_CMD_1 takes 3 arguments, 4 given",
}


def check(binary, dictionary, fixture, out):
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import harness
    os.makedirs(out, exist_ok=True)
    with open(fixture) as file:
        expected = json.load(file)
    assert [case["typed"] for case in expected] == LINES, "the fixture is not of these lines: record it again"
    gds = harness.Gds(binary, dictionary, out)
    different = []
    try:
        with sync_playwright() as playwright:
            browser = playwright.chromium.launch()
            problems = []
            page = harness.open_page(browser, gds.url, problems)
            harness.show(page, "commands")
            before = None
            for case in expected:
                page.fill("#command-string", case["typed"])
                page.press("#command-string", "Tab")
                page.wait_for_timeout(100)
                shown = page.evaluate(NEW_SHOWN)
                if case["typed"] in EXCESS:
                    # The Python page drops the arguments in excess and loads the rest (deviations.md, finding 54):
                    # fgds refuses the line, leaves it as typed and the form as it was. The line the Python page
                    # shows is then typed, so that the next cases start from the same form.
                    refused = dict(before, line=case["typed"], error=EXCESS[case["typed"]])
                    if shown != refused:
                        different.append((case["typed"], refused, shown))
                    page.fill("#command-string", case["line"])
                    page.press("#command-string", "Tab")
                    page.wait_for_timeout(100)
                    shown = page.evaluate(NEW_SHOWN)
                before = shown
                wanted = {key: case[key] for key in shown}
                wanted["inputs"], shown["inputs"] = plain(wanted["inputs"]), plain(shown["inputs"])
                if shown != wanted:
                    different.append((case["typed"], wanted, shown))
            # An empty field stays empty (the Python page: `undefined is not a command.`).
            page.fill("#command-string", "")
            page.press("#command-string", "Tab")
            page.wait_for_timeout(100)
            empty = page.evaluate(NEW_SHOWN)
            assert empty["line"] == "" and empty["error"].startswith("Expected command string of the form"), empty
            # A line that is not a command, then the send button: nothing is sent. (The Python page sends the
            # command its form still holds, CMD_NO_OP at the start: deviations.md.)
            posted = []
            page.on("request", lambda r: posted.append(r.url) if r.method == "POST" and r.url.endswith("/commands") else None)
            for text in ("CdhCore.cmdDisp.CMD_NO_OP_STRING, \"unfinished", "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1, 1.5, 0x1F",
                         "CdhCore.cmdDisp.CMD_TEST_CMD_1, 1", "No.such.COMMAND, 1", ""):
                page.reload()
                page.wait_for_selector("body[data-ready=true]")
                harness.show(page, "commands")
                assert page.input_value("#command-select") == "CdhCore.cmdDisp.CMD_NO_OP", "CMD_NO_OP is not the command at the start"
                page.fill("#command-string", text)
                page.press("#command-string", "Tab")
                page.click("#command-send")
                page.wait_for_timeout(300)
                assert not posted, f"{text!r} then the send button: a command was sent"
            page.reload()
            page.wait_for_selector("body[data-ready=true]")
            harness.show(page, "commands")
            page.click("#command-send")
            page.wait_for_timeout(300)
            assert len(posted) == 1, "the send button alone does not send CMD_NO_OP: the check above proves nothing"
            # No Ref is connected: that command is answered 503, link down, which is not an error of the page.
            problems[:] = [text for text in problems if "503" not in text]
            assert not problems, problems
            browser.close()
    finally:
        gds.stop()
    for typed, wanted, shown in different:
        print(f"  !! {typed}\n     Python: {json.dumps(wanted)}\n     fgds:   {json.dumps(shown)}")
    assert not different, f"{len(different)} of {len(expected)} lines differ from the Python GDS page"
    quoted = sum('"' in line for line in LINES)
    harness.step(f"Command String: {len(expected)} lines typed ({quoted} with quoted strings; arrays, structs, enums, a name "
                 "in lower case, too few and too many arguments, an unknown command, a wrong constant, lines that are not of the form) "
                 "fill the form, show the line and the message as the Python GDS page; a line with too many arguments is refused and left as typed")


if __name__ == "__main__":
    if sys.argv[1] == "record":
        record(*sys.argv[2:])
    else:
        check(*sys.argv[2:])
