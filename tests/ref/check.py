#!/usr/bin/env python3
"""`fgds run` against a running Ref (item 2 of the slice): commands through the API, the
resulting events and channel samples read back through the record API and the WebSocket.

Usage: check.py <fgds binary> <Ref binary> <dictionary> <directory for logs> [framing profile]

The framing profile (`ccsds` or `fprime`, the default) is the one Ref was built with.
"""
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402

FGDS, REF, DICTIONARY, OUT = sys.argv[1:5]
FRAMING = sys.argv[5] if len(sys.argv) > 5 else "fprime"
os.makedirs(OUT, exist_ok=True)
HTTP, LINK = scenario.free_port(), scenario.free_port()
API = f"http://127.0.0.1:{HTTP}/api/v1"


def get(path):
    with urllib.request.urlopen(API + path, timeout=10) as response:
        return json.load(response)


def post(path, body):
    request = urllib.request.Request(API + path, json.dumps(body).encode(), {"content-type": "application/json"})
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def records(query):
    return get(f"/records?{query}&limit=10000")["records"]


def step(text):
    print(f"  ok: {text}")


def Stream(query):
    return scenario.Stream(HTTP, query)


fgds = scenario.start(
    [FGDS, "run", "--dictionary", DICTIONARY, "--bind", f"127.0.0.1:{HTTP}", "--logs", f"{OUT}/logs", "--comm-tcp-address", f"127.0.0.1:{LINK}",
     "--framing", FRAMING],
    open(f"{OUT}/fgds.log", "w"),
)
ref = None
try:
    scenario.wait_for("fgds", lambda: get("/version"))
    ref = scenario.start([REF, "-a", "127.0.0.1", "-p", str(LINK)], open(f"{OUT}/ref.log", "w"), cwd=OUT)
    scenario.wait_for("telemetry from Ref", lambda: records("kind=fprime.channel"))
    assert get("/status")["link"]["state"] == "connected", get("/status")
    step("fgds run: Ref connected, telemetry arriving")
    start = int(get("/cursor")["seq"])

    sent = [post("/commands", {"name": name, "args": args}) for name, args in scenario.COMMANDS[:2]]
    opcodes = [r["id"] for r in records(f"kind=fprime.command&after_seq={start}")]
    assert len(opcodes) == 2, opcodes
    scenario.wait_for("the commands to complete", lambda: len([
        r for r in records(f"kind=fprime.event&after_seq={start}") if r["body"]["name"].endswith("OpCodeCompleted")]) == 2)
    step(f"CMD_NO_OP and CMD_NO_OP_STRING(\"hello\") sent through the API as records {[s['seq'] for s in sent]}")

    events = records(f"kind=fprime.event&after_seq={start}")
    texts = {e["body"]["name"].split(".")[-1]: e["body"]["display_text"] for e in events}
    assert texts["NoOpReceived"] == "Received a NO-OP command", texts
    assert texts["NoOpStringReceived"] == "Received a NO-OP string=hello", texts
    assert all(e["flight_time"]["seconds"] > 0 and e["ground_time"].endswith("Z") for e in events)
    step(f"record API: {len(events)} events, among them {texts['NoOpReceived']!r} and {texts['NoOpStringReceived']!r}")

    name = "CdhCore.cmdDisp.CommandsDispatched"
    count = scenario.wait_for("the command counter", lambda: [
        r for r in records(f"kind=fprime.channel&after_seq={start}&filter=name:eq:{name}") if int(r["body"]["value"]) >= 2])
    latest = get("/records/latest?kind=fprime.channel")["records"]
    assert any(r["body"]["name"] == name for r in latest)
    step(f"record API: channel {name} = {count[-1]['body']['value']}; latest value of {len(latest)} channels")

    # Replay then live on one socket: everything since `start` is already stored when the
    # stream opens; the third command is sent only after the replay has been read.
    stream = Stream(f"after_seq={start}")
    cursor = int(get("/cursor")["seq"])
    replayed = []
    while not replayed or int(replayed[-1]["seq"]) < cursor:
        replayed.append(stream.next())
    name, args = scenario.COMMANDS[2]
    third = post("/commands", {"name": name, "args": args})
    live = stream.until(lambda r: r["kind"] == "fprime.event" and r["body"]["name"].endswith("OpCodeCompleted"))
    numbers = [int(r["seq"]) for r in replayed + live]
    assert numbers == list(range(start + 1, start + 1 + len(numbers))), "the stream repeats or skips a record"
    assert int(third["seq"]) > cursor and third["seq"] in [r["seq"] for r in live]
    replayed_events = [r["seq"] for r in replayed if r["kind"] == "fprime.event"]
    assert [e["seq"] for e in events] == replayed_events[:len(events)], "the replay differs from the record API"
    kinds = sorted({r["kind"] for r in live})
    assert {"fprime.command", "fprime.event"} <= set(kinds), kinds
    step(f"WebSocket from sequence number {start}: {len(replayed)} records replayed, then {len(live)} live "
         f"({', '.join(kinds)}) after {name}{tuple(args)}; numbers {numbers[0]} to {numbers[-1]} without a gap or a repeat")

    filtered = Stream(f"after_seq={start}&kind=fprime.event")
    again = [filtered.next()["seq"] for _ in range(len(replayed_events))]
    assert again == replayed_events
    step("WebSocket with kind=fprime.event: the same events, in the same order")

    # What the framing layers know about each packet, on the stored records (LNK-19).
    with open(DICTIONARY) as file:
        constants = {c["qualifiedName"]: c["value"] for c in json.load(file).get("constants", [])}
    decoded = records("kind=fprime.event") + records("kind=fprime.channel")
    if FRAMING == "fprime":
        assert all(not r["meta"] for r in decoded), [r for r in decoded if r["meta"]][:3]
        step(f"F Prime framing: no packet metadata on {len(decoded)} event and channel records")
    else:
        scid = str(constants["ComCfg.SpacecraftId"])
        assert all(r["meta"]["ccsds.scid"] == scid and r["meta"]["ccsds.vcid"] == "1" for r in decoded), decoded[:3]
        apids = {kind: {r["meta"]["ccsds.apid"] for r in records(f"kind={kind}")}
                 for kind in ("fprime.event", "fprime.channel")}
        # ComCfg.Apid: FW_PACKET_LOG is 2, FW_PACKET_TELEM is 1.
        assert apids == {"fprime.event": {"2"}, "fprime.channel": {"1"}}, apids
        # Events are one per Space Packet: their sequence counts have no gap.
        counts = [int(r["meta"]["ccsds.seq_count"]) for r in records("kind=fprime.event")]
        assert counts == list(range(counts[0], counts[0] + len(counts))), counts
        step(f"CCSDS framing: records carry ccsds.scid {scid}, ccsds.vcid 1, ccsds.apid (events 2, channels 1) "
             f"and ccsds.seq_count ({counts[0]} to {counts[-1]} on events, no gap)")

    problems = records("kind=gds.decode_error") + records("kind=gds.unknown_packet") + records("kind=raw.discarded")
    assert not problems, problems[:3]
    step("no decode error, unknown packet or discarded byte during the run")
    print("ref: all checks passed")
finally:
    for process in (ref, fgds):
        if process:
            scenario.stop(process)
