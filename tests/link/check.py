#!/usr/bin/env python3
"""No byte of a connection is lost by `fgds`, from its first one.

Ref is started against the stock `fgds` through a proxy that keeps the bytes, several times. Each time:

  1. the raw log holds the bytes the proxy passed on, all of them and in order;
  2. the event records are the event packets of those bytes, one for one, by identifier.

Ref sometimes prints its two version events and does not write them to the socket (README.md): the check
counts those starts and compares with the bytes, not with what Ref printed.

Usage: check.py <fgds binary> <Ref binary> <dictionary> <directory for output> [starts]
"""
import glob
import json
import os
import shutil
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
import scenario  # noqa: E402

FGDS, REF, DICTIONARY, OUT = sys.argv[1:5]
STARTS = int(sys.argv[5]) if len(sys.argv) > 5 else 8
FRAME = scenario.tm_frame_size(DICTIONARY)
VERSION_EVENTS = (0x1003000, 0x1003001)
IDLE_APID, EVENT_PACKET = 0x7FF, b"\x00\x02"


def get(url):
    with urllib.request.urlopen(url, timeout=10) as response:
        return json.load(response)


def event_ids(stream):
    """The identifiers of the event packets in a stream of TM frames, in order."""
    found = []
    for frame in scenario.frames(stream, "ccsds", FRAME):
        at, end = 6, FRAME - 2
        while at + 6 <= end:
            apid = int.from_bytes(frame[at:at + 2], "big") & 0x7FF
            size = int.from_bytes(frame[at + 4:at + 6], "big") + 1
            if apid == IDLE_APID:
                break
            data = frame[at + 6:at + 6 + size]
            if data[:2] == EVENT_PACKET:
                found.append(int.from_bytes(data[2:6], "big"))
            at += 6 + size
    return found


def one(out):
    os.makedirs(out)
    link, http = scenario.free_port(), scenario.free_port()
    api = f"http://127.0.0.1:{http}/api/v1"
    gds = scenario.start([FGDS, "run", "--dictionary", DICTIONARY, "--bind", f"127.0.0.1:{http}", "--logs", f"{out}/logs",
                          "--database", f"{out}/gds.db", "--comm-tcp-address", f"127.0.0.1:{link}"],
                         open(f"{out}/fgds.log", "w"), cwd=out)
    try:
        scenario.wait_for("fgds", lambda: get(f"{api}/version"))
        proxy = scenario.Proxy(link)
        ref = scenario.start_ref(REF, proxy, out)
        scenario.wait_for("three frames from Ref", lambda: len(proxy.downlink) >= 3 * FRAME)
        scenario.stop(ref)

        def raw():
            return b"".join(bytes.fromhex(line.split()[2]) for path in glob.glob(f"{out}/logs/*/raw.log")
                            for line in open(path) if line.split()[1] == "recv")
        scenario.wait_for("the raw log", lambda: len(raw()) >= len(proxy.downlink), 10)
        time.sleep(0.5)
        wire = bytes(proxy.downlink)
        assert raw() == wire, f"{out}: the raw log has {len(raw())} bytes, Ref wrote {len(wire)}"
        sent = event_ids(wire)
        scenario.wait_for("the event records", lambda: len(get(
            f"{api}/records?kind=fprime.event&after_seq=0&limit=10000")["records"]) >= len(sent), 10)
        records = [r["id"] for r in get(f"{api}/records?kind=fprime.event&after_seq=0&limit=10000")["records"]]
        assert records == sent, f"{out}: events sent {sent}, recorded {records}"
        printed = open(f"{out}/ref.log", errors="replace").read()
        assert all(f"({number})" in printed for number in VERSION_EVENTS), f"{out}: Ref did not print its version events"
        return len(wire), len(sent), all(number in sent for number in VERSION_EVENTS)
    finally:
        scenario.stop(gds)


def restart(out):
    """fgds killed (SIGKILL) with Ref connected listens on the same port again at once."""
    os.makedirs(out)
    link, http = scenario.free_port(), scenario.free_port()
    api = f"http://127.0.0.1:{http}/api/v1"
    command = [FGDS, "run", "--dictionary", DICTIONARY, "--bind", f"127.0.0.1:{http}", "--logs", f"{out}/logs",
               "--database", f"{out}/gds.db", "--comm-tcp-address", f"0.0.0.0:{link}"]
    gds = scenario.start(command, open(f"{out}/fgds.log", "w"), cwd=out)
    ref = None
    try:
        scenario.wait_for("fgds", lambda: get(f"{api}/version"))
        ref = scenario.start([REF, "-a", "127.0.0.1", "-p", str(link)], open(f"{out}/ref.log", "w"), cwd=out)
        scenario.wait_for("the link", lambda: get(f"{api}/records?kind=fprime.channel&limit=1")["records"])
        gds.kill()
        gds.wait()
        began = time.monotonic()
        gds = scenario.start(command, open(f"{out}/fgds.log", "a"), cwd=out)
        before = len(get_when_up(api))

        def again():
            assert gds.poll() is None, open(f"{out}/fgds.log").read()[-400:]
            return len(get(f"{api}/records?kind=fprime.channel&after_seq=0&limit=10000")["records"]) > before
        scenario.wait_for("telemetry after the restart", again, 5)
        took = time.monotonic() - began
        assert took < 5 and "Address already in use" not in open(f"{out}/fgds.log").read(), took
        return took
    finally:
        for process in (ref, gds):
            if process:
                scenario.stop(process)


def get_when_up(api):
    return scenario.wait_for("fgds again", lambda: get(f"{api}/records?kind=fprime.channel&after_seq=0&limit=10000"), 5)["records"]


shutil.rmtree(OUT, ignore_errors=True)
print(f"  ok: fgds killed with Ref connected and started again: listening on the same port and receiving telemetry "
      f"from Ref after {restart(f'{OUT}/restart'):.1f} s")
results = [one(f"{OUT}/{index}") for index in range(STARTS)]
without = sum(1 for _, _, version in results if not version)
print(f"  ok: {STARTS} starts of Ref through a proxy: the raw log is the {sum(r[0] for r in results)} bytes Ref wrote, "
      f"and the {sum(r[1] for r in results)} event packets in them are the event records; Ref printed its two version "
      f"events every time and wrote them to the socket in {STARTS - without} of {STARTS} starts")
print("link: all checks passed")
