#!/usr/bin/env python3
"""The same file uplinked many times, each time to a fresh fgds and a fresh Ref connected directly or through a relay.

Ref has no flow control for file packets: its file uplink queue holds 10 and a packet that does not
fit ends Ref with an assert. Every upload must arrive whole, with Ref still running.
"""
import argparse
import os
import shutil
import socket
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402
from check import Fgds, content, read  # noqa: E402

SIZE = 30_000


HOLD = 0.03


def relay_to(gds_port):
    """A relay that holds what it reads for HOLD seconds, so Ref gets what was sent in that time at once,
    as it may from a network or a loaded machine. Returns the port Ref connects to."""
    server = socket.create_server(("127.0.0.1", 0))

    def pump(source, sink):
        try:
            while data := source.recv(65536):
                time.sleep(HOLD)
                sink.sendall(data)
        except OSError:
            pass
        for end in (source, sink):
            try:
                end.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass

    def run():
        ref, _ = server.accept()
        gds = socket.create_connection(("127.0.0.1", gds_port))
        threading.Thread(target=pump, args=(gds, ref), daemon=True).start()
        pump(ref, gds)

    threading.Thread(target=run, daemon=True).start()
    return server.getsockname()[1]


def once(args, out, relay, extra=(), size=SIZE):
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(f"{out}/local")
    data = content("repeat", size)
    with open(f"{out}/local/repeat.bin", "wb") as file:
        file.write(data)
    gds = Fgds(args, out, f"{out}/store", extra)
    ref = None
    try:
        # Half of the runs through a relay.
        port = relay_to(gds.link_port) if relay else gds.link_port
        ref = scenario.start([args.ref, "-a", "127.0.0.1", "-p", str(port)], open(f"{out}/ref.log", "w"), cwd=out)
        scenario.wait_for("the link", lambda: gds.get("/files/uplink")["link"] == "connected")
        gds.cli("upload", "local/repeat.bin", "--destination", "repeat.bin")
        started = time.monotonic()
        ended = scenario.wait_for("the uplink to end", lambda: gds.uplink("repeat.bin")["state"] not in ("queued", "transferring")
                                  and gds.uplink("repeat.bin"), 60)
        took = time.monotonic() - started
        time.sleep(0.5)  # Ref writes the last packets after the ground has sent them
        crashed = ref.poll() is not None or "Assert" in open(f"{out}/ref.log", errors="replace").read()
        received = read(f"{out}/repeat.bin") if os.path.exists(f"{out}/repeat.bin") else b""
        return {"state": ended["state"], "crashed": crashed, "equal": received == data, "received": len(received), "took": took}
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)


def main():
    parser = argparse.ArgumentParser()
    for name in ("fgds", "ref", "dictionary", "out"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--framing", default="ccsds")
    parser.add_argument("--times", type=int, default=20)
    args = parser.parse_args()
    for name in ("fgds", "ref", "dictionary", "out"):
        setattr(args, name, os.path.abspath(getattr(args, name)))
    runs = [once(args, f"{args.out}/repeat/{n}", n % 2 == 0) for n in range(args.times)]
    bad = [(n, run) for n, run in enumerate(runs) if run["crashed"] or not run["equal"] or run["state"] != "finished"]
    assert not bad, f"{len(bad)} of {len(runs)} uploads went wrong: {bad}"
    took = sorted(run["took"] for run in runs)
    print(f"  ok: {SIZE} bytes uplinked {len(runs)} times to a fresh Ref, half of them through a relay that holds "
          f"bytes for {HOLD} s: no crash, every file identical, every transfer finished ({took[0]:.2f} to {took[-1]:.2f} s each)")
    big = [once(args, f"{args.out}/repeat/big{n}", n % 2 == 0, size=100_003) for n in range(3)]
    bad = [run for run in big if run["crashed"] or not run["equal"] or run["state"] != "finished"]
    assert not bad, f"100003-byte uploads went wrong: {bad}"
    print(f"  ok: 100003 bytes uplinked 3 times, sent at once without a resume: no crash, every file identical "
          f"({min(r['took'] for r in big):.2f} to {max(r['took'] for r in big):.2f} s each)")
    # Without the pause between packets the relay may hand Ref more packets than its queue holds, and Ref
    # then ends (it depends on the machine): whatever happens, only a whole file is reported as finished.
    lost = [once(args, f"{args.out}/repeat/lost{n}", True, ["--files-cooldown", "0"]) for n in range(3)]
    for run in lost:
        assert (run["state"] == "finished") == (run["equal"] and not run["crashed"]), run
    gone = [run for run in lost if run["crashed"]]
    print(f"  ok: with no pause between packets Ref ended in {len(gone)} of {len(lost)} uploads "
          f"and the reported states are {[run['state'] for run in lost]}: finished only for a whole file")

main()
