"""What the browser checks share: the processes, the page, and the table scrapers."""
import json
import os
import sys
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402


class Gds:
    """A running GDS binary, with the HTTP port of its page and the TCP port Ref connects to."""

    def __init__(self, binary, dictionary, out, env=None, framing="ccsds", extra=()):
        http, self.link_port = scenario.free_port(), scenario.free_port()
        self.url = f"http://127.0.0.1:{http}"
        self.process = scenario.start(
            [binary, "run", "--framing", framing, "--logs", f"{out}/logs", "--dictionary", dictionary, "--bind", f"127.0.0.1:{http}",
             "--comm-tcp-address", f"127.0.0.1:{self.link_port}", *extra],
            open(f"{out}/gds.log", "w"), env={**os.environ, **(env or {})},
        )
        scenario.wait_for("the GDS", lambda: self.get("/version"))

    def get(self, path):
        with urllib.request.urlopen(f"{self.url}/api/v1{path}", timeout=10) as response:
            return json.load(response)

    def post(self, path, body):
        request = urllib.request.Request(
            f"{self.url}/api/v1{path}", json.dumps(body).encode(), {"content-type": "application/json"})
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    def stop(self):
        scenario.stop(self.process)


def step(text):
    print(f"  ok: {text}")


def open_page(browser, url, problems):
    """A page of the new UI, ready: every tab module loaded. Page errors are collected."""
    page = browser.new_page(viewport={"width": 1400, "height": 850})
    page.on("pageerror", lambda error: problems.append(str(error)))
    page.on("console", lambda message: problems.append(message.text) if message.type == "error" else None)
    page.goto(url)
    page.wait_for_selector("body[data-ready=true]")
    return page


def show(page, tab):
    page.click(f"#tabs button[data-tab={tab}]")
    page.wait_for_selector(f"#tabs button[data-tab={tab}][aria-selected=true]")


# The cells of a table of the new UI, by column name.
ROWS = """(table) => [...table.querySelectorAll("tbody tr")].map((row) =>
    Object.fromEntries([...row.querySelectorAll("td[data-col]")].map((cell) => [cell.dataset.col, cell.textContent.trim()])))"""


def rows(page, table):
    return page.eval_on_selector(table, ROWS)
