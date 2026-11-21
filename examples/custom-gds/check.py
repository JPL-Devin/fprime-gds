#!/usr/bin/env python3
"""Show what the example binary proves (item 4 of the slice), with this script as the flight side.

Usage: check.py <directory of the built binaries> <dictionary>
"""
import json
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request
import zlib

BIN, DICTIONARY = sys.argv[1], sys.argv[2]
TIME = bytes([0, 2, 0, 0, 0, 0, 100, 0, 0, 0, 7])


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def run(binary, *args):
    return subprocess.run([f"{BIN}/{binary}", *args], capture_output=True, text=True)


def fprime_frame(packet):
    frame = struct.pack(">II", 0xDEADBEEF, len(packet)) + packet
    return frame + struct.pack(">I", zlib.crc32(frame))


def station_frame(data, station):
    return b"GS" + struct.pack(">BH", station, len(data)) + data


def step(text):
    print(f"  ok: {text}")


# Startup failure on a duplicate registration: both plugins are named.
conflict = run("conflict", "check", "--dictionary", DICTIONARY)
assert conflict.returncode != 0, "the conflicting binary started"
assert "notes" in conflict.stderr and "other-notes" in conflict.stderr and "example.note" in conflict.stderr, conflict.stderr
step(f"duplicate registration stops startup: {conflict.stderr.strip()}")

# The subcommand, and the build without one default plugin.
kinds = run("custom-gds", "kinds")
assert kinds.returncode == 0 and "example.note: A note written by an operator" in kinds.stdout, kinds
assert "channel" not in kinds.stdout
step("subcommand `kinds` lists example.note and no channel kind")
check = run("custom-gds", "check", "--dictionary", DICTIONARY, "--framing", "station-fprime")
assert check.returncode == 0, check.stderr
assert "framing: station > fprime" in check.stdout and "channels 0.1.0" not in check.stdout, check.stdout
step("`check` shows framing station > fprime and no channels plugin")

http, link = free_port(), free_port()
api = f"http://127.0.0.1:{http}"
gds = subprocess.Popen(
    [f"{BIN}/custom-gds", "run", "--dictionary", DICTIONARY, "--framing", "station-fprime",
     "--bind", f"127.0.0.1:{http}", "--tcp-address", f"127.0.0.1:{link}"],
    stderr=subprocess.DEVNULL,
)


def call(path, body=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(api + path, data, {"content-type": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def get(path):
    return json.loads(call(path)[1])


def wait_for(what, condition):
    for _ in range(100):
        try:
            value = condition()
            if value:
                return value
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {what}")


try:
    wait_for("the server", lambda: call("/api/v1/version")[0] == 200)
    flight = wait_for("the link", lambda: socket.create_connection(("127.0.0.1", link), timeout=5))
    wait_for("link up", lambda: get("/api/v1/status")["link"]["state"] == "connected")

    # The added framing layer in use, both directions.
    event = bytes([0, 2, 0, 0, 2, 0]) + TIME  # event 512, T.c.NoOp
    flight.sendall(b"noise" + station_frame(fprime_frame(event), 3))
    events = wait_for("the event", lambda: get("/api/v1/records?kind=event")["records"])
    assert events[0]["body"]["name"] == "T.c.NoOp" and events[0]["meta"] == {"station": "3"}, events
    assert get("/api/v1/records?kind=raw.discarded")["records"], "the noise was not recorded"
    step("downlink through station + fprime framing; station number kept as packet metadata")
    assert call("/api/v1/commands", {"name": "T.c.NO_OP"})[0] == 200
    sent = flight.recv(100)
    assert sent == station_frame(fprime_frame(bytes([0, 0, 0, 0, 1, 0])), 1), sent.hex()
    step("uplink through fprime + station framing")

    # Without the channels plugin: no route, and a channel packet is an unknown packet.
    assert call("/api/v1/channels/definitions")[0] == 404
    flight.sendall(station_frame(fprime_frame(bytes([0, 1, 0, 0, 3, 0]) + TIME + bytes(4)), 3))
    wait_for("the unknown packet", lambda: get("/api/v1/records?kind=gds.unknown_packet")["records"])
    step("built without the channels plugin: its route is absent, its packets are recorded as unknown")

    # The added record kind: stored, and queryable through its route and the record API.
    assert call("/api/v1/notes", {"text": "Antenna stowed", "author": "ops"})[0] == 200
    assert call("/api/v1/notes", {"text": "Pass complete", "author": "lead"})[0] == 200
    assert get("/api/v1/notes") == ["Antenna stowed", "Pass complete"]
    found = get("/api/v1/records?kind=example.note&text=antenna")["records"]
    assert [r["body"]["author"] for r in found] == ["ops"], found
    found = get("/api/v1/records?kind=example.note&filter=author:eq:lead")["records"]
    assert [r["body"]["text"] for r in found] == ["Pass complete"], found
    step("record kind example.note stored; queried by its route, by text and by declared field")

    # The tab: listed, and its module and the page shell are served.
    tabs = get("/ui/tabs")
    assert tabs == [{"id": "notes", "title": "Notes", "module": "/ui/plugins/notes/notes.js"}], tabs
    assert b'customElements.define("notes-tab"' in call(tabs[0]["module"])[1]
    assert b"/ui/shell.js" in call("/")[1] and b"import(tab.module)" in call("/ui/shell.js")[1]
    step("tab listed by /ui/tabs; its module and the page shell that loads it are served")

    # The contained panic: recorded, shown in the status, and everything else keeps working.
    assert call("/api/v1/notes", {"text": "panic"})[0] == 200
    faults = wait_for("the fault", lambda: get("/api/v1/records?kind=gds.plugin_fault")["records"])
    assert faults[0]["body"]["plugin"] == "faulty", faults
    assert get("/api/v1/status")["plugins"]["faulty"]["fault"]
    assert call("/api/v1/notes", {"text": "Still here"})[0] == 200
    assert get("/api/v1/notes")[-1] == "Still here" and gds.poll() is None
    step(f"panic contained: {faults[0]['body']['message']!r}; the GDS keeps serving")
finally:
    gds.terminate()
    gds.wait(timeout=10)
print("example: all shown")
