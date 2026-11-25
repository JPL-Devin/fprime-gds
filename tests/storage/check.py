#!/usr/bin/env python3
"""The stock `fgds` with its default storage (SQLite) against a running Ref:

  1. the GDS is killed (SIGKILL) while records arrive: the database holds every record up to a last one,
     and what is missing after it is no more than the queue and one batch (STO-11; the age of the last
     stored record is reported, it depends on the load of the machine);
  2. restarted on the same database: a new run, no sequence number used twice; the history
     of both runs, then the live stream, without a gap or a repeat;
  3. restarted with a changed dictionary: a warning, and the earlier runs keep their own
     dictionary and their records as decoded at the time;
  4. `fgds run --replay <database>`: the runs are served without a link and the file is
     not changed.

Usage: check.py <fgds binary> <Ref binary> <dictionary> <directory for output>
"""
import datetime
import hashlib
import json
import os
import shutil
import signal
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "compare"))
import scenario  # noqa: E402

FGDS, REF, DICTIONARY, OUT = sys.argv[1:5]
shutil.rmtree(OUT, ignore_errors=True)
os.makedirs(OUT)
DATABASE = f"{OUT}/gds.db"
LINK = scenario.free_port()
NO_OP = {"name": "CdhCore.cmdDisp.CMD_NO_OP", "args": []}


def step(text):
    print(f"  ok: {text}")


def milliseconds(iso):
    return datetime.datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp() * 1000


class Gds:
    """One `fgds run` and, unless `replay`, a Ref connected to it."""

    count = 0

    def __init__(self, dictionary=DICTIONARY, replay=False):
        Gds.count += 1
        self.http = scenario.free_port()
        self.api = f"http://127.0.0.1:{self.http}/api/v1"
        self.log = f"{OUT}/fgds-{Gds.count}.log"
        options = ["--bind", f"127.0.0.1:{self.http}"]
        if replay:
            options += ["--replay", DATABASE]
        else:
            options += ["--dictionary", dictionary, "--database", DATABASE, "--logs", f"{OUT}/logs",
                        "--comm-tcp-address", f"127.0.0.1:{LINK}"]
        self.process = scenario.start([FGDS, "run", *options], open(self.log, "w"))
        self.ref = None
        try:
            scenario.wait_for("fgds", lambda: self.get("/version"))
            if not replay:
                self.ref = scenario.start([REF, "-a", "127.0.0.1", "-p", str(LINK)],
                                          open(f"{OUT}/ref-{Gds.count}.log", "w"), cwd=OUT)
                scenario.wait_for("telemetry from Ref", lambda: self.records("kind=fprime.channel"))
        except BaseException:
            self.stop()
            raise

    def get(self, path):
        with urllib.request.urlopen(self.api + path, timeout=10) as response:
            return json.load(response)

    def post(self, path, body):
        request = urllib.request.Request(self.api + path, json.dumps(body).encode(), {"content-type": "application/json"})
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    def records(self, query):
        """Every page of the query."""
        found, after = [], ""
        while True:
            page = self.get(f"/records?{query}&limit=10000{after}")
            found += page["records"]
            if not page.get("next"):
                return found
            after = f"&after_seq={page['next']}"

    def kill(self):
        """SIGKILL: nothing is flushed or closed."""
        os.kill(self.process.pid, signal.SIGKILL)
        self.process.wait()
        self.stop()

    def stop(self):
        if self.ref:
            scenario.stop(self.ref)
            self.ref = None
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            self.process.wait(timeout=20)


def increasing(numbers):
    return all(a < b for a, b in zip(numbers, numbers[1:]))


# 1. Killed while records arrive.
gds = Gds()
try:
    first_run = gds.get("/cursor")["run"]
    old_dictionary = gds.get("/dictionary")
    gds.post("/commands", NO_OP)
    time.sleep(0.5)
    before_kill = gds.records("kind=fprime.event")
    # Commands until the kill, so records are arriving when it happens.
    answered = {}
    end = time.monotonic() + 1.5
    while time.monotonic() < end:
        seq = int(gds.post("/commands", NO_OP)["seq"])
        answered[seq] = time.time() * 1000
        time.sleep(0.004)  # about 200 commands a second: more overflows the command queue of Ref
    killed_at = time.time() * 1000
    gds.kill()
finally:
    gds.stop()

# 2. Restarted on the same database and dictionary.
gds = Gds()
try:
    runs = gds.get("/runs")
    second_run = gds.get("/cursor")["run"]
    assert [r["run"] for r in runs] == [first_run, second_run], runs
    first = gds.records(f"run={first_run}")
    numbers = [int(r["seq"]) for r in first]
    assert increasing(numbers)
    # A command is answered when its record has a number, before the commit (uplink
    # records do not wait for storage): the last ones may be lost, within the bound.
    missing = sorted(set(answered) - set(numbers))
    # Records are stored in order: what the kill lost is the end, at most the queue (1,000) and one batch (500).
    holes = [seq for seq in missing if seq < numbers[-1]]
    assert not holes, f"records before the last stored one are not in the database: {holes[:5]}"
    assert len(missing) <= 1500, f"{len(missing)} answered records are not in the database"
    stored_events = {r["seq"] for r in first if r["kind"] == "fprime.event"}
    assert {r["seq"] for r in before_kill} <= stored_events, "records read before the kill are gone"
    age = killed_at - milliseconds(first[-1]["ground_time"])
    step(f"killed with SIGKILL after {len(answered)} commands: the {len(missing)} last answered are "
         f"missing from the {len(first)} records of run {first_run} in the database; the last record is {max(age, 0):.0f} ms older than the kill")

    first_seq = int(runs[1]["first_seq"])
    assert first_seq > numbers[-1], (first_seq, numbers[-1])
    sent = int(gds.post("/commands", NO_OP)["seq"])
    scenario.wait_for("the command to complete", lambda: [
        r for r in gds.records("kind=fprime.event") if r["body"]["name"].endswith("OpCodeCompleted")])
    second = [int(r["seq"]) for r in gds.records("")]
    assert second[0] >= first_seq and sent in second and not set(second) & set(numbers), (first_seq, second[:3])
    assert second == list(range(second[0], second[0] + len(second))), "run 2 skips or repeats a number"
    step(f"restarted: run {second_run} begins at sequence number {first_seq}, after the {numbers[-1]} of the killed "
         f"run; no number is used twice")

    # History of both runs, then live from where the history ended.
    history = gds.records("run=all")
    cursor = int(history[-1]["seq"])
    stream = scenario.Stream(gds.http, f"after_seq={cursor}")
    gds.post("/commands", NO_OP)
    live = stream.until(lambda r: r["kind"] == "fprime.event" and r["body"]["name"].endswith("OpCodeCompleted"))
    both = [int(r["seq"]) for r in history + live]
    assert increasing(both), "the history then the stream repeats a record"
    assert [int(r["seq"]) for r in history] == numbers + [n for n in both if first_seq <= n <= cursor]
    assert int(live[0]["seq"]) == cursor + 1 and both[-1] - cursor == len(live), "the stream skips a record"
    assert {r["run"] for r in history} == {first_run, second_run}
    step(f"history of runs {first_run} and {second_run} ({len(history)} records), then {len(live)} live records: "
         f"no gap and no repeat across the restart")
    assert not gds.get("/status")["warnings"], gds.get("/status")
    raw_logs = [os.path.join(d, f) for d, _, files in os.walk(f"{OUT}/logs") for f in files if f.startswith("raw")]
    assert len(raw_logs) == 2 and all(os.path.getsize(f) > 0 for f in raw_logs), raw_logs
    step("each run has a log directory with the raw bytes of the link")
finally:
    gds.stop()

# 3. Restarted with another dictionary.
changed = json.loads(open(DICTIONARY).read())
changed["metadata"]["projectVersion"] = "changed"
for event in changed["events"]:
    if event["name"].endswith("NoOpReceived"):
        event["format"] = "A different text"
with open(f"{OUT}/changed.json", "w") as file:
    json.dump(changed, file)
gds = Gds(dictionary=f"{OUT}/changed.json")
try:
    third_run = gds.get("/cursor")["run"]
    warnings = gds.get("/status")["warnings"]
    assert any("differs from the dictionary of run" in w for w in warnings), warnings
    assert gds.get(f"/dictionary?run={first_run}") == old_dictionary
    assert gds.get("/dictionary")["metadata"]["projectVersion"] == "changed"
    assert gds.get(f"/dictionary?run={third_run}")["metadata"]["projectVersion"] == "changed"
    gds.post("/commands", NO_OP)
    text = lambda run: {r["body"]["display_text"] for r in gds.records(f"run={run}&kind=fprime.event")  # noqa: E731
                        if r["body"]["name"].endswith("NoOpReceived")}
    scenario.wait_for("the event", lambda: text(third_run))
    assert text(third_run) == {"A different text"} and text(first_run) == {"Received a NO-OP command"}
    hashes = [r["dictionary_hash"] for r in gds.get("/runs")]
    assert hashes[0] == hashes[1] != hashes[2], hashes
    step(f"changed dictionary: warning {warnings[0]!r}; run {first_run} keeps its dictionary and its event texts")
    everything = [r["seq"] for r in gds.records("run=all")]
finally:
    gds.stop()

# 4. The database alone, read-only.
digest = lambda: hashlib.sha256(open(DATABASE, "rb").read()).hexdigest()  # noqa: E731
before = digest()
files = sorted(os.listdir(OUT))
gds = Gds(replay=True)
try:
    assert [r["run"] for r in gds.get("/runs")] == [first_run, second_run, third_run]
    assert gds.get("/dictionary")["metadata"]["projectVersion"] == "changed"
    assert gds.get(f"/dictionary?run={first_run}") == old_dictionary
    replayed = [r["seq"] for r in gds.records("run=all")]
    assert replayed[:len(everything)] == everything and len(replayed) >= len(everything)
    assert gds.get("/status")["link"]["state"] != "connected"
    try:
        gds.post("/commands", NO_OP)
        raise AssertionError("a command was accepted by a read-only GDS")
    except urllib.error.HTTPError as error:
        refusal = json.load(error)["error"]["message"]
finally:
    gds.stop()
assert digest() == before, "the database changed while it was open read-only"
assert sorted(os.listdir(OUT)) == sorted(files + [os.path.basename(gds.log)]), "files were created beside the database"
assert "gds.db-wal" not in files, "the write-ahead log remains after a GDS that was stopped properly"
step(f"fgds run --replay: {len(replayed)} records of 3 runs served with the stored dictionary; "
     f"a command is refused ({refusal}); the file is unchanged")
