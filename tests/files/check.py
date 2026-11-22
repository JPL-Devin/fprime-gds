#!/usr/bin/env python3
"""File uplink and downlink of fgds against Ref, compared with the Python GDS.

Runs in the fprime virtual environment (the Python GDS side needs it). See README.md.
"""
import argparse
import base64
import hashlib
import json
import os
import random
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import zlib

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402

UPLINKS = [("up.bin", 100_001), ("one.bin", 1), ("empty.bin", 0)]
DOWN_SIZE = 70_003
FILE_PACKET = 3


def content(name, size):
    return random.Random(name).randbytes(size)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read(path):
    with open(path, "rb") as file:
        return file.read()


def file_packets(stream, framing="fprime", scid=0):
    return [p for p in scenario.uplink_packets(stream, framing, scid) if int.from_bytes(p[:2], "big") == FILE_PACKET]


def ok(text):
    print(f"  ok: {text}")


def product_of(ref_dir):
    return scenario.wait_for("a data product file on the Ref side", lambda: sorted(
        f for f in os.listdir(f"{ref_dir}/DpCat") if f.endswith(".fdp")), 30)[0]


def ground_name(product):
    """The name both ground systems give the product: dpCat sends it as ./DpCat/<file>."""
    return "._DpCat_" + product


def prepare(out):
    """The directory Ref runs in: the file to downlink and the data product directory."""
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(f"{out}/DpCat")
    with open(f"{out}/down-source.bin", "wb") as file:
        file.write(content("down", DOWN_SIZE))


class Capture:
    """The uplink bytes of a proxy up to now."""

    def __init__(self, proxy):
        self.uplink = bytes(proxy.uplink)


def sequence_counts(args, out, proxy):
    """The Space Packet sequence count of the file packets sent, after files went both ways.

    The Python GDS and `Ref` each keep one count per application identifier for both directions: the count of
    a file packet sent up is the number of file packets that went up or down before it. `Ref` reports a count
    it does not expect as `UnexpectedSequenceCount`.
    """
    if args.framing != "ccsds":
        return None
    size, down = scenario.tm_frame_size(args.dictionary), 0
    for frame in scenario.frames(bytes(proxy.downlink), "ccsds", size):
        at = 6
        while at + 6 <= size - 2 and (apid := int.from_bytes(frame[at:at + 2], "big") & 0x7FF) != 0x7FF:
            down += apid == FILE_PACKET
            at += 6 + int.from_bytes(frame[at + 4:at + 6], "big") + 1
    up = []
    for frame in scenario.tc_frames(bytes(proxy.uplink), scenario.spacecraft_id(args.dictionary)):
        data, at = frame[5:-2], 0
        while at + 6 <= len(data):
            if int.from_bytes(data[at:at + 2], "big") & 0x7FF == FILE_PACKET:
                up.append(int.from_bytes(data[at + 2:at + 4], "big") & 0x3FFF)
            at += 6 + int.from_bytes(data[at + 4:at + 6], "big") + 1
    assert "UnexpectedSequenceCount" not in open(f"{out}/ref.log", errors="replace").read(), \
        f"{out}: Ref reported an unexpected sequence count"
    return {"up": len(up), "down": down, "last": up[-1], "expected": (len(up) - 1 + down) % 16384}


def results(args, out, proxy, down_dir, product, skip=0):
    """What is compared: the file packets sent, the files on the Ref side, the files downlinked."""
    return {
        "uplink_packets": [p.hex() for p in file_packets(proxy.uplink, args.framing, scenario.spacecraft_id(args.dictionary))][skip:],
        "ref_files": {name: digest(read(f"{out}/{name}")) for name, _ in UPLINKS},
        "downlinked": {name: [len(data), digest(data)] for name in sorted(os.listdir(down_dir))
                       for data in [read(f"{down_dir}/{name}")]},
        "product": product,
        "product_on_ref": digest(read(f"{out}/DpCat/{product}")),
        "product_size": os.path.getsize(f"{out}/DpCat/{product}"),
    }


# ---------------------------------------------------------------- the Python GDS

class HandshakeMissed(Exception):
    """The uplinker of the Python GDS stopped: it sends a packet before it notes the handshake to expect."""


def python_side(args, out, staging, attempts=3):
    """The run of the Python GDS, begun again from a new Ref when its uplinker missed a handshake.

    `FileUplinker.send` (fprime_gds/common/files/uplinker.py) hands the packet to the encoder and only then
    sets the handshake it expects; the handshake of a start packet (sent from the queue thread) can return
    in between, is ignored, nothing more is sent and the file ends as TIMEOUT after 20 s. No fgds process is
    part of this.
    """
    for attempt in range(1, attempts + 1):
        try:
            return python_once(args, out, staging)
        except HandshakeMissed as missed:
            print(f"  note: Python GDS, attempt {attempt}: {missed}; its uplinker missed a handshake", flush=True)
            if attempt == attempts:
                raise


def python_once(args, out, staging):
    from fprime_gds.executables.cli import ConfigDrivenParser, StandardPipelineParser

    prepare(out)
    gds_port, tts_port = scenario.free_port(), scenario.free_port()
    shared = ["--dictionary", args.dictionary, "--no-zmq", "--tts-port", str(tts_port), "--tts-addr", "127.0.0.1",
              "--file-storage-directory", f"{out}/files"]
    gds = scenario.start(
        [shutil.which("fprime-gds"), "-n", "-g", "none", "--ip-address", "127.0.0.1", "--ip-port", str(gds_port),
         "--framing-selection", "fprime" if args.framing == "fprime" else "space-packet-space-data-link", "--logs", f"{out}/gds-logs", *shared],
        open(f"{out}/gds.log", "w"),
    )
    ref = pipeline = None
    try:
        scenario.wait_for("the Python GDS link port",
                          lambda: socket.create_connection(("127.0.0.1", gds_port)).close() or True)
        proxy = scenario.Proxy(gds_port)
        parsed, _, _ = ConfigDrivenParser.parse_known_args(
            [StandardPipelineParser], arguments=[*shared, "--logs", f"{out}/client-logs"], client=True)
        pipeline = StandardPipelineParser.pipeline_factory(parsed)
        ref = scenario.start_ref(args.ref, proxy, out)
        scenario.wait_for("telemetry", lambda: pipeline.histories.channels.retrieve())
        uplinker, downlinker = pipeline.files.uplinker, pipeline.files.downlinker

        for name, size in UPLINKS:
            os.makedirs(staging, exist_ok=True)
            with open(f"{staging}/{name}", "wb") as file:
                file.write(content(name, size))
            uplinker.enqueue(f"{staging}/{name}", name)

            def uplinked():
                states = [f["state"] for f in uplinker.current_files() if f["destination"] == name]
                if "TIMEOUT" in states:
                    raise HandshakeMissed(f"the uplink of {name} ended as TIMEOUT")
                return "FINISHED" in states

            scenario.wait_for(f"the uplink of {name}", uplinked, 60)

        def downlinked(name):
            return [f for f in downlinker.current_files()
                    if os.path.basename(f["destination"]) == name and f["state"] == "FINISHED"]

        pipeline.send_command("FileHandling.fileDownlink.SendFile", ["down-source.bin", "down.bin"])
        scenario.wait_for("the downlink of down.bin", lambda: downlinked("down.bin"), 60)
        pipeline.send_command("DataProducts.dpCat.BUILD_CATALOG", [])
        time.sleep(1)
        pipeline.send_command("Ref.dpDemo.Dp", ["IMMEDIATE", "10", "PROC_TYPE_NONE"])
        product = product_of(out)
        time.sleep(1)
        pipeline.send_command("DataProducts.dpCat.START_XMIT_CATALOG", ["NO_WAIT", "false"])
        scenario.wait_for("the downlink of the data product", lambda: downlinked(ground_name(product)), 60)
        time.sleep(0.5)
        found = results(args, out, Capture(proxy), f"{out}/files/fprime-downlink", product)
        with open(f"{staging}/after.bin", "wb") as file:
            file.write(content("after.bin", 300))
        uplinker.enqueue(f"{staging}/after.bin", "after.bin")
        scenario.wait_for("the uplink of after.bin", lambda: [
            f for f in uplinker.current_files() if f["destination"] == "after.bin" and f["state"] == "FINISHED"], 60)
        time.sleep(1)
        found["sequence"] = sequence_counts(args, out, proxy)
        return found
    finally:
        try:
            if pipeline:
                pipeline.disconnect()
        finally:
            for process in (ref, gds):
                if process:
                    scenario.stop(process)
                    try:  # the comm process of the Python GDS outlives its parent otherwise
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass


# ---------------------------------------------------------------- fgds

class Fgds:
    def __init__(self, args, out, store, extra=(), framing=None):
        os.makedirs(out, exist_ok=True)
        http, self.link_port = scenario.free_port(), scenario.free_port()
        self.root = f"http://127.0.0.1:{http}"
        self.api = f"{self.root}/api/v1"
        self.binary, self.out = args.fgds, out
        self.process = scenario.start(
            [args.fgds, "run", "--framing", framing or args.framing, "--logs", f"{out}/logs", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}",
             "--comm-tcp-address", f"127.0.0.1:{self.link_port}", "--files-directory", store, *extra],
            open(f"{out}/fgds.log", "w"),
        )
        scenario.wait_for("fgds", lambda: self.get("/version"))

    def request(self, path, body=None, method=None, root=False):
        request = urllib.request.Request((self.root if root else self.api) + path, body, method=method)
        if body is not None and method is None:
            request.add_header("content-type", "application/json")
        return urllib.request.urlopen(request, timeout=20)

    def get(self, path):
        with self.request(path) as response:
            return json.load(response)

    def post(self, path, body=None):
        with self.request(path, json.dumps(body or {}).encode()) as response:
            return json.load(response)

    def status_of(self, path, body=None):
        """The HTTP status of a request that may be refused."""
        try:
            with self.request(path, body, "POST" if body is not None else "GET") as response:
                return response.status
        except urllib.error.HTTPError as error:
            return error.code

    def records(self, kind):
        found, after = [], 0
        while after is not None:
            page = self.get(f"/records?kind={kind}&after_seq={after}&limit=10000")
            found += page["records"]
            after = page["next"]
        return found

    def cli(self, *arguments, check=True):
        """`fgds files ...` against this GDS."""
        # An upload returns at once here; `file_uplink` below is the waiting form.
        if arguments[0] == "upload":
            arguments = (*arguments, "--no-wait")
        done = subprocess.run([self.binary, "files", "--url", self.root, *arguments],
                              capture_output=True, text=True, cwd=self.out, timeout=60)
        if check:
            assert done.returncode == 0, f"fgds files {arguments}: {done.stdout}{done.stderr}"
        return done

    def uplink(self, name):
        return [f for f in self.get("/files/uplink")["files"] if f["name"] == name][-1]

    def downlink(self, name, state="finished"):
        return [f for f in self.get("/files/downlink")["transfers"] if f["name"] == name and f["state"] == state]

    def healthy(self, log):
        status = self.get("/status")
        faults = {name: p["fault"] for name, p in status["plugins"].items() if p.get("fault")}
        assert not faults, f"faulted plugins: {faults}"
        assert "panicked" not in open(log).read(), f"a panic is in {log}"


def rust_side(args, out, store):
    prepare(out)
    gds = Fgds(args, out, store)
    ref = None
    try:
        queue = gds.get("/files/uplink")
        assert not queue["paused"] and queue["link"] != "connected", queue
        assert gds.status_of("/files/uplink/resume", b"{}") == 503, "resume must be refused without a link"
        # A file given before the first connection waits and goes once Ref connects, with no resume.
        os.makedirs(f"{out}/local")
        with open(f"{out}/local/early.bin", "wb") as file:
            file.write(content("early.bin", 700))
        gds.cli("upload", "local/early.bin", "--destination", "early.bin")
        time.sleep(0.5)
        assert gds.uplink("early.bin")["state"] == "queued"
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(args.ref, proxy, out)
        scenario.wait_for("telemetry", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])

        scenario.wait_for("the file given before the link", lambda: gds.uplink("early.bin")["state"] == "finished", 30)
        assert read(f"{out}/early.bin") == content("early.bin", 700)
        early = len(file_packets(proxy.uplink, args.framing, scenario.spacecraft_id(args.dictionary)))
        assert not gds.get("/files/uplink")["paused"]
        ok("the queue runs from the start: a file given before Ref connected was sent once it did, with no resume; "
           "resume is refused while the link is not connected")
        # Queued with `fgds files upload` while paused, then resumed: the queue order is the order sent.
        gds.cli("pause")
        # A file that is not there: the line and exit code of `fprime-cli file-uplink`.
        missing = gds.cli("upload", "local/none.bin", check=False)
        assert (missing.returncode, missing.stdout, missing.stderr) == (1, "Error: File not found at local/none.bin\n", ""), missing
        for name, size in UPLINKS:
            with open(f"{out}/local/{name}", "wb") as file:
                file.write(content(name, size))
            gds.cli("upload", f"local/{name}", "--destination", name)
        assert [f["state"] for f in gds.get("/files/uplink")["files"] if f["name"] != "early.bin"] == ["queued"] * len(UPLINKS)
        assert "paused" in gds.cli("queue").stdout
        gds.cli("resume")
        for name, size in UPLINKS:
            done = scenario.wait_for(f"the uplink of {name}", lambda: gds.uplink(name)["state"] == "finished" and gds.uplink(name), 60)
            assert done["done"] == done["size"] == size, done

        gds.post("/commands", {"name": "FileHandling.fileDownlink.SendFile", "args": ["down-source.bin", "down.bin"]})
        scenario.wait_for("the downlink of down.bin", lambda: gds.downlink("down.bin"), 60)
        # A part of a file: the start packet announces the whole file. Finished, with the bytes
        # received as the progress, as the Python GDS shows it (FINISHED, 25%).
        gds.post("/commands", {"name": "FileHandling.fileDownlink.SendPartial", "args": ["down-source.bin", "part.bin", 100, DOWN_SIZE // 4]})
        part = scenario.wait_for("the downlink of part.bin", lambda: gds.downlink("part.bin"), 60)[-1]
        assert part["size"] == DOWN_SIZE and 0 < part["done"] < DOWN_SIZE and part["started"] <= part["ended"], part
        assert part["state"] == "finished" and part["problems"] == 0, part
        assert read(f"{store}/downlink/part.bin")[100:] == content("down", DOWN_SIZE)[100:100 + DOWN_SIZE // 4]
        ok(f"SendPartial: part.bin finished with {part['done']} of {part['size']} bytes ({part['done'] * 100 // part['size']}%), the bytes of the source")
        gds.post("/commands", {"name": "DataProducts.dpCat.BUILD_CATALOG", "args": []})
        time.sleep(1)
        gds.post("/commands", {"name": "Ref.dpDemo.Dp", "args": ["IMMEDIATE", 10, "PROC_TYPE_NONE"]})
        product = product_of(out)
        time.sleep(1)
        gds.post("/commands", {"name": "DataProducts.dpCat.START_XMIT_CATALOG", "args": ["NO_WAIT", False]})
        scenario.wait_for("the downlink of the data product", lambda: gds.downlink(ground_name(product)), 60)
        found = results(args, out, proxy, f"{store}/downlink", product, skip=early)
        # A file sent up after files came down: the sequence count goes on from both.
        with open(f"{out}/local/after.bin", "wb") as file:
            file.write(content("after.bin", 300))
        gds.cli("upload", "local/after.bin", "--destination", "after.bin")
        scenario.wait_for("the uplink of after.bin", lambda: gds.uplink("after.bin")["state"] == "finished", 60)
        time.sleep(1)
        found["sequence"] = sequence_counts(args, out, proxy)
        # After the packets compared with the Python GDS were taken.
        file_uplink(gds, out)

        # The routes and `fgds files` give the same files.
        listing = gds.get("/files/downlink")
        assert {f["name"]: f["size"] for f in listing["files"]} == {n: v[0] for n, v in found["downlinked"].items()}
        with gds.request("/files/downlink/down.bin") as response:
            assert response.read() == content("down", DOWN_SIZE)
            assert response.headers["content-disposition"].startswith("attachment"), response.headers
            assert response.headers["x-content-type-options"] == "nosniff"
            assert response.headers["content-type"] == "application/octet-stream"
        gds.cli("download", ground_name(product), "--output", "product-copy.fdp")
        assert read(f"{out}/product-copy.fdp") == read(f"{out}/DpCat/{product}")
        assert "down.bin" in gds.cli("list").stdout
        ok("routes and `fgds files`: upload, queue, resume, list, download (attachment, nosniff)")

        # File content is in files only.
        assert gds.records("fprime.file.packet") == [], "file packets are in the record store"
        needle_up, needle_down = content("up.bin", 100_001)[:64], content("down", DOWN_SIZE)[:64]
        kinds = [kind["id"] for kind in gds.get("/kinds")]
        assert "fprime.file.uplink" in kinds and "raw.sent" in kinds, kinds
        for name in kinds:
            text = json.dumps(gds.records(name))
            for needle in (needle_up, needle_down):
                assert needle.hex() not in text and base64.b64encode(needle[:48]).decode() not in text, \
                    f"file content found in records of kind {name}"
        progress = gds.records("fprime.file.uplink") + gds.records("fprime.file.downlink")
        assert {r["body"]["state"] for r in progress} == {"queued", "transferring", "finished"}, progress[:3]
        assert all(r["entity"] for r in progress)
        ok(f"file content is not in the record store; {len(progress)} progress records, each with an entity")

        for tab in ("uplink", "downlink"):
            with gds.request(f"/ui/plugins/{tab}/{tab}.js", root=True) as response:
                assert b"customElements.define" in response.read()
        ok("the Uplink and Downlink tab modules are served")

        gds.healthy(f"{out}/fgds.log")
        return found, gds, ref, proxy
    except BaseException:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)
        raise


def file_uplink(gds, out):
    """`fgds file-uplink` and `fgds files upload`: the lines, the wait and the exit codes of `fprime-cli file-uplink`."""
    with open(f"{out}/local/waited.bin", "wb") as file:
        file.write(content("waited.bin", 5000))
    receipt = (r"Flight software confirmed receipt: \d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{6}: "
               r"FileHandling\.fileUplink\.FileReceived \(83886082\) \(2\(0\)-\d+:\d+\) EventSeverity\.ACTIVITY_HI : Received file %s\n")
    for command, name in ((["file-uplink", "--url", gds.root, "local/waited.bin", "waited-1.bin"], "waited-1.bin"),
                          (["files", "--url", gds.root, "upload", "local/waited.bin", "-d", "waited-2.bin"], "waited-2.bin")):
        done = subprocess.run([gds.binary, *command], capture_output=True, text=True, cwd=out, timeout=90)
        assert (done.returncode, done.stderr) == (0, ""), done
        lines = re.sub(r"Uplinking 'waited\.bin': \d+%\n", "", done.stdout)
        expected = (re.escape(f"Queued 'local/waited.bin' for uplink to '{name}'\n"
                              f"All uplink packets sent for 'waited.bin' to '{name}'\n") + receipt % re.escape(name))
        assert re.fullmatch(expected, lines), done.stdout
        # It returned after the flight side had the file.
        assert read(f"{out}/{name}") == content("waited.bin", 5000)
    quick = subprocess.run([gds.binary, "file-uplink", "--url", gds.root, "--no-wait", "local/waited.bin"],
                           capture_output=True, text=True, cwd=out, timeout=30)
    assert (quick.returncode, quick.stdout) == (0, "Queued 'local/waited.bin' for uplink to '/waited.bin'\n"), quick
    scenario.wait_for("the file queued without waiting", lambda: gds.uplink("waited.bin")["state"] == "finished", 60)
    gds.cli("pause")
    late = subprocess.run([gds.binary, "file-uplink", "--url", gds.root, "-t", "2", "local/waited.bin", "late.bin"],
                          capture_output=True, text=True, cwd=out, timeout=30)
    assert (late.returncode, late.stdout) == (1, "Queued 'local/waited.bin' for uplink to 'late.bin'\n"
                                              "Error: uplink of 'waited.bin' did not complete within 2.0 seconds\n"), late
    gds.cli("resume")
    scenario.wait_for("the file that was held", lambda: os.path.exists(f"{out}/late.bin") and read(f"{out}/late.bin") == content("waited.bin", 5000), 60)
    missing = subprocess.run([gds.binary, "file-uplink", "--url", gds.root, "local/none.bin"], capture_output=True, text=True, cwd=out)
    assert (missing.returncode, missing.stdout, missing.stderr) == (1, "Error: File not found at local/none.bin\n", ""), missing
    ok("fgds file-uplink and fgds files upload print the lines of fprime-cli file-uplink (queued, all packets sent, the "
       "FileReceived event) and return once the flight side has the file; --no-wait returns at once; a timeout gives exit code 1")


def link_loss(gds, ref, out):
    """FIL-4: Ref goes away during an uplink."""
    with open(f"{out}/local/big.bin", "wb") as file:
        file.write(content("big", 3_000_000))
    gds.cli("upload", "local/big.bin", "--destination", "big.bin")
    scenario.wait_for("the uplink of big.bin to start", lambda: gds.uplink("big.bin")["done"] > 0, 30)
    gds.cli("upload", "local/one.bin", "--destination", "later.bin")
    scenario.stop(ref)
    failed = scenario.wait_for("the uplink to fail", lambda: gds.uplink("big.bin")["state"] == "failed" and gds.uplink("big.bin"), 30)
    queue = gds.get("/files/uplink")
    assert queue["paused"] and gds.uplink("one.bin")["state"] == "queued", queue
    assert gds.status_of("/files/uplink/resume", b"{}") == 503
    assert gds.cli("resume", check=False).returncode != 0
    ok(f"FIL-4: link lost during an uplink: it ended as failed ({failed['reason']}), the queue paused "
       "with the next file kept, resume refused")
    identifier = gds.uplink("one.bin")["id"]
    gds.cli("cancel", identifier)
    assert gds.uplink("one.bin")["state"] == "removed"
    ok("FIL-1: a queued file is removed with `fgds files cancel`")


# ---------------------------------------------------------------- fgds with a flight side made of bad packets

def frame(packet):
    data = scenario.START_WORD + struct.pack(">I", len(packet)) + packet
    return data + struct.pack(">I", zlib.crc32(data))


def start(seq, size, source, destination):
    return struct.pack(">HBII", FILE_PACKET, 0, seq, size) + bytes([len(source)]) + source + bytes([len(destination)]) + destination


def data(seq, offset, chunk):
    return struct.pack(">HBIIH", FILE_PACKET, 1, seq, offset, len(chunk)) + chunk


def end(seq, chunks):
    whole = b"".join(chunks)
    whole += bytes(-len(whole) % 4)
    checksum = sum(struct.unpack(f">{len(whole) // 4}I", whole)) & 0xFFFFFFFF
    return struct.pack(">HBII", FILE_PACKET, 2, seq, checksum)


def cancel(seq):
    return struct.pack(">HBI", FILE_PACKET, 3, seq)


def bad_packets(args, out):
    shutil.rmtree(out, ignore_errors=True)
    store = f"{out}/store"
    # No pause between uplink packets: the buffers of a flight side that does not read fill at once.
    gds = Fgds(args, out, store, ["--files-timeout", "1.5", "--files-cooldown", "0"], framing="fprime")
    try:
        flight = scenario.wait_for("the link", lambda: socket.create_connection(("127.0.0.1", gds.link_port)))
        a, b, c = b"A" * 300, b"B" * 300, b"C" * 77
        good = start(0, 677, b"/src/good.bin", b"good.bin")
        cases = [
            # (what, packets, expected state of the transfer or None, expected text in a report)
            ("a good file", [good, data(1, 0, a), data(2, 300, b), data(3, 600, c), end(4, [a, b, c])], ("good.bin", "finished"), None),
            ("data without a start", [data(1, 0, a)], None, "data packet without a transfer"),
            ("end without a start", [end(2, [a])], None, "end packet without a transfer"),
            ("cancel without a start", [cancel(3)], None, "cancel packet without a transfer"),
            ("unknown file packet type", [struct.pack(">HBI", FILE_PACKET, 9, 0)], None, "unknown file packet type 9"),
            ("truncated start", [good[:11]], None, "truncated file packet"),
            ("truncated data", [data(1, 0, a)[:9]], None, "truncated file packet"),
            ("data shorter than its length", [data(1, 0, a)[:-5]], None, "says 300 bytes and holds 295"),
            ("truncated end", [end(1, [a])[:8]], None, "truncated file packet"),
            ("bytes after an end packet", [end(1, [a]) + b"x"], None, "after the end"),
            ("empty file packet", [struct.pack(">H", FILE_PACKET)], None, "truncated file packet"),
            ("duplicated data", [start(0, 677, b"s", b"dup.bin"), data(1, 0, a), data(1, 0, a), data(2, 300, b), data(3, 600, c), end(4, [a, b, c])],
             ("dup.bin", "finished"), "duplicated or out of order"),
            ("out-of-order data", [start(0, 677, b"s", b"order.bin"), data(2, 300, b), data(1, 0, a), data(3, 600, c), end(4, [a, b, c])],
             ("order.bin", "finished"), "duplicated or out of order"),
            ("a lost data packet", [start(0, 677, b"s", b"lost.bin"), data(1, 0, a), data(3, 600, c), end(4, [a, b, c])],
             ("lost.bin", "failed"), "packets were lost"),
            ("corrupted data", [start(0, 677, b"s", b"corrupt.bin"), data(1, 0, a), data(2, 300, b"X" + b[1:]), data(3, 600, c), end(4, [a, b, c])],
             ("corrupt.bin", "failed"), None),
            ("data past the file size", [start(0, 10, b"s", b"past.bin"), data(1, 0, a), end(2, [a])], ("past.bin", "failed"), "ends after the file size"),
            # Fewer bytes than announced, as `SendPartial` sends: finished, as the Python GDS shows it.
            ("a file shorter than announced", [start(0, 677, b"s", b"short.bin"), data(1, 0, a), end(2, [a])], ("short.bin", "finished"), None),
            ("a canceled file", [start(0, 677, b"s", b"cancel.bin"), data(1, 0, a), cancel(2)], ("cancel.bin", "canceled"), None),
            ("a start during a transfer", [start(0, 677, b"s", b"first.bin"), data(1, 0, a), start(0, 77, b"s", b"second.bin"), data(1, 0, c), end(2, [c])],
             ("first.bin", "failed"), "start packet during a transfer"),
            ("a name outside the storage", [start(0, 77, b"s", b"../../../outside.bin"), data(1, 0, c), end(2, [c])], (".._.._.._outside.bin", "finished"), None),
            ("an unusable name", [start(0, 77, b"s", b".."), data(1, 0, c), end(2, [c])], ("", "failed"), None),
            ("an empty name", [start(0, 77, b"s", b""), data(1, 0, c), end(2, [c])], ("", "failed"), None),
        ]
        for what, sent, expected, report in cases:
            before = len(gds.records("fprime.file.problem")) + len(gds.records("gds.decode_error"))
            received = gds.get("/status")["counters"].get("packets_received", 0)
            flight.sendall(b"".join(frame(p) for p in sent))
            scenario.wait_for(what, lambda: gds.get("/status")["counters"].get("packets_received", 0) == received + len(sent))
            time.sleep(0.2)
            reports = gds.records("fprime.file.problem") + gds.records("gds.decode_error")
            texts = [json.dumps(r["body"]) for r in reports]
            new = len(reports) - before
            if expected:
                name, state = expected
                transfers = [t for t in gds.get("/files/downlink")["transfers"] if t["name"] == name and t["state"] == state]
                assert transfers, f"{what}: no {state} transfer named {name!r}: {gds.get('/files/downlink')['transfers'][-2:]}"
                reason = transfers[-1]["reason"]
            if report:
                assert any(report in t for t in texts), f"{what}: no report with {report!r} in {texts[-3:]}"
            assert expected is None or state in ("finished", "canceled") or reason or report, what
            gds.healthy(f"{out}/fgds.log")
            detail = f"transfer {state}" + (f" ({reason})" if reason else "") if expected else "no transfer"
            ok(f"{what}: {detail}, {new} report(s)")
        assert sorted(os.listdir(f"{store}/downlink")) == sorted(
            ["good.bin", "dup.bin", "order.bin", "lost.bin", "corrupt.bin", "past.bin", "short.bin", "cancel.bin",
             "first.bin", "second.bin", ".._.._.._outside.bin"])
        assert not os.path.exists(f"{out}/outside.bin") and not os.path.exists(f"{store}/outside.bin")
        whole = a + b + c
        for name in ("good.bin", "dup.bin", "order.bin"):
            assert read(f"{store}/downlink/{name}") == whole, name
        ok("FIL-3: every file is directly inside the downlink directory; reordered and duplicated data gives the right file")

        for path in ("/files/downlink/..%2F..%2Ffgds.log", "/files/downlink/%2Fetc%2Fpasswd", "/files/downlink/..", "/files/downlink/nothing"):
            assert gds.status_of(path) == 404, path
        os.symlink("/etc/passwd", f"{store}/downlink/link.bin")
        assert gds.status_of("/files/downlink/link.bin") == 404
        os.remove(f"{store}/downlink/link.bin")
        assert gds.status_of("/files/uplink?name=..", b"x") == 422
        ok("FIL-3: download of a path outside the directory or of a link, and upload of an unusable name, are refused")

        flight.sendall(frame(start(0, 677, b"s", b"timeout.bin")) + frame(data(1, 0, a)))
        timed = scenario.wait_for("the time-out", lambda: [t for t in gds.get("/files/downlink")["transfers"] if t["name"] == "timeout.bin" and t["state"] == "failed"], 10)
        ok(f"FIL-2: a downlink without data ends as failed ({timed[0]['reason']})")

        # FIL-5 and FIL-1 with a flight side that stops reading: no progress, then the next file.
        assert gds.get("/files/uplink")["link"] == "connected"
        os.makedirs(f"{out}/local")
        for name, size in (("stuck.bin", 30_000_000), ("next.bin", 10)):
            with open(f"{out}/local/{name}", "wb") as file:
                file.write(content(name, size))
            gds.cli("upload", f"local/{name}")
        gds.cli("resume")
        stuck = scenario.wait_for("the uplink without progress to fail", lambda: gds.uplink("stuck.bin")["state"] == "failed" and gds.uplink("stuck.bin"), 60)
        assert "no progress" in stuck["reason"], stuck
        ok(f"FIL-5: an uplink the flight side does not read ends as failed ({stuck['reason']}) after {stuck['done']} bytes")
        gds.healthy(f"{out}/fgds.log")
    finally:
        scenario.stop(gds.process)


def cancel_and_pause(args, out):
    """FIL-1 against a flight side that reads: cancel one file, pause during another; the cancel packets are on the link."""
    shutil.rmtree(out, ignore_errors=True)
    gds = Fgds(args, out, f"{out}/store", ["--files-cooldown", "0.02"], framing="fprime")
    try:
        flight = scenario.wait_for("the link", lambda: socket.create_connection(("127.0.0.1", gds.link_port)))
        received = bytearray()
        import threading

        def pump():
            try:
                while chunk := flight.recv(65536):
                    received.extend(chunk)
            except OSError:
                pass
        threading.Thread(target=pump, daemon=True).start()
        os.makedirs(f"{out}/local")
        for name in ("a.bin", "b.bin", "c.bin"):
            with open(f"{out}/local/{name}", "wb") as file:
                file.write(content(name, 50_000 if name != "c.bin" else 600))
            gds.cli("upload", f"local/{name}", "-d", f"/flight/{name}")
        scenario.wait_for("the link state", lambda: gds.get("/files/uplink")["link"] == "connected")
        gds.cli("resume")
        scenario.wait_for("a.bin to start", lambda: gds.uplink("a.bin")["done"] > 0)
        gds.cli("cancel", gds.uplink("a.bin")["id"])
        scenario.wait_for("a.bin to be canceled", lambda: gds.uplink("a.bin")["state"] == "canceled")
        scenario.wait_for("b.bin to start", lambda: gds.uplink("b.bin")["done"] > 0)
        gds.cli("pause")
        scenario.wait_for("b.bin to be canceled", lambda: gds.uplink("b.bin")["state"] == "canceled")
        time.sleep(0.5)
        assert gds.uplink("c.bin")["state"] == "queued" and gds.get("/files/uplink")["paused"]
        gds.cli("resume")
        scenario.wait_for("c.bin to finish", lambda: gds.uplink("c.bin")["state"] == "finished")
        time.sleep(0.3)
        kinds = [p[2] for p in file_packets(received)]
        sequences = [int.from_bytes(p[3:7], "big") for p in file_packets(received)]
        assert kinds.count(0) == 3 and kinds.count(3) == 2 and kinds.count(2) == 1, kinds
        assert kinds[-5:] == [0, 1, 1, 1, 2] and sequences[-5:] == [0, 1, 2, 3, 4], (kinds[-5:], sequences[-5:])
        chunks = [len(p) - 13 for p in file_packets(received) if p[2] == 1]
        assert set(chunks[:-1]) <= {256, 88} and chunks[-3:] == [256, 256, 88], chunks[-3:]
        ok(f"FIL-1: cancel and pause each end the active uplink with a cancel packet on the link; "
           f"after resume the next file is sent ({len(kinds)} file packets, {kinds.count(3)} cancel)")
        gds.healthy(f"{out}/fgds.log")
    finally:
        scenario.stop(gds.process)


def main():
    parser = argparse.ArgumentParser()
    for name in ("fgds", "ref", "dictionary", "out"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--framing", choices=["ccsds", "fprime"], default="ccsds", help="the framing Ref was built with")
    args = parser.parse_args()
    # The source path is in the start packet: both ground systems read the file from the same path.
    store = f"{args.out}/store"
    shutil.rmtree(store, ignore_errors=True)

    python = python_side(args, f"{args.out}/python", f"{store}/uplink")
    rust, gds, ref, _ = rust_side(args, f"{args.out}/rust", store)
    try:
        json.dump({"python": python, "rust": rust}, open(f"{args.out}/results.json", "w"), indent=1)
        assert len(python["uplink_packets"]) == len(rust["uplink_packets"]), \
            (len(python["uplink_packets"]), len(rust["uplink_packets"]))
        for index, (p, r) in enumerate(zip(python["uplink_packets"], rust["uplink_packets"])):
            assert p == r, f"file packet {index} differs:\n python {p}\n fgds   {r}"
        ok(f"uplink: {len(rust['uplink_packets'])} file packets on the link, byte-identical to the Python GDS "
           f"({sum(len(p) // 2 for p in rust['uplink_packets'])} bytes)")
        if args.framing == "ccsds":
            for side, found in (("python", python), ("fgds", rust)):
                counts = found["sequence"]
                assert counts["down"] > 0 and counts["last"] == counts["expected"], (side, counts)
            ok(f"sequence count of a file packet sent after files came down: python {python['sequence']['last']} "
               f"after {python['sequence']['up'] - 1} up and {python['sequence']['down']} down, fgds "
               f"{rust['sequence']['last']} after {rust['sequence']['up'] - 1} up and {rust['sequence']['down']} down; "
               "Ref reported no unexpected sequence count with either")
        expected = {name: digest(content(name, size)) for name, size in UPLINKS}
        assert python["ref_files"] == rust["ref_files"] == expected
        ok(f"uplink: the {len(UPLINKS)} files on the Ref side are identical with both and equal to the originals "
           f"(sizes {[s for _, s in UPLINKS]})")
        want = [DOWN_SIZE, digest(content("down", DOWN_SIZE))]
        assert python["downlinked"]["down.bin"] == rust["downlinked"]["down.bin"] == want
        ok(f"downlink (fileDownlink.SendFile): down.bin, {DOWN_SIZE} bytes, sha256 {want[1][:16]}..., same with both")
        for side, found in (("python", python), ("fgds", rust)):
            size, sha = found["downlinked"][ground_name(found["product"])]
            assert sha == found["product_on_ref"] and size == found["product_size"], side
        assert python["product_size"] == rust["product_size"]
        assert sorted(python["downlinked"]) == sorted(["down.bin", ground_name(python["product"])])
        assert sorted(rust["downlinked"]) == sorted(["down.bin", "part.bin", ground_name(rust["product"])])
        ok(f"data product (dpDemo.Dp, dpCat.START_XMIT_CATALOG): python {python['product']}, fgds {rust['product']}; "
           f"each downlinked as ._DpCat_<name>, {rust['product_size']} bytes, identical to the file Ref wrote")
        link_loss(gds, ref, f"{args.out}/rust")
        gds.healthy(f"{args.out}/rust/fgds.log")
    finally:
        for process in (ref, gds.process):
            scenario.stop(process)
    bad_packets(args, f"{args.out}/bad")
    cancel_and_pause(args, f"{args.out}/control")
    print("files: all passed")


if __name__ == "__main__":
    main()
