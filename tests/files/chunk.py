#!/usr/bin/env python3
"""The largest uplink chunk size of each framing, against Ref: the largest the framing carries arrives whole,
one byte more is refused before anything is sent, by the command line, the page route and `fgds run`."""
import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402
from check import FILE_PACKET, Fgds, content, file_packets, ok, prepare, read  # noqa: E402

# F´ framing: the largest frame the flight software accepts is an option, 2048 bytes as the stock Ref
# (`ComFprime.frameAccumulatorSize`, not in the dictionary), that is 2023 bytes of file data.
LARGEST = {"ccsds": 997, "fprime": 2023}
# The largest F´ frame of all (a packet of 4095 bytes), for flight software that takes it.
FPRIME_FRAME, FPRIME_CHUNK = 4107, 4082


def main():
    parser = argparse.ArgumentParser()
    for name in ("fgds", "ref", "dictionary", "out"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--framing", choices=["ccsds", "fprime"], default="ccsds")
    args = parser.parse_args()
    out, largest = f"{args.out}/chunk-{args.framing}", LARGEST[args.framing]
    on_ref = largest
    prepare(out)
    os.makedirs(f"{out}/local")
    size = 3 * on_ref + 10
    with open(f"{out}/local/big.bin", "wb") as file:
        file.write(content("big.bin", size))

    # `fgds run` and `fgds check` refuse the option alike, with the largest size in the message.
    for subcommand in ("run", "check"):
        done = subprocess.run([args.fgds, subcommand, "--framing", args.framing, "--dictionary", args.dictionary, "--storage", "memory",
                               "--bind", f"127.0.0.1:{scenario.free_port()}", "--comm-tcp-address", f"127.0.0.1:{scenario.free_port()}",
                               "--files-chunk-size", str(largest + 1)], capture_output=True, text=True, timeout=30)
        assert done.returncode != 0 and f"at most {largest} bytes of file data" in done.stderr, (subcommand, done)
        # The F´ framing names what sets its limit; the CCSDS framing has its own number and nothing to add.
        assert ("frameAccumulatorSize of ComFprime: --framing-fprime-flight-frame-size" in done.stderr) == (args.framing == "fprime"), done

    if args.framing == "fprime":
        check = lambda frame, chunk: subprocess.run(
            [args.fgds, "check", "--framing", "fprime", "--dictionary", args.dictionary, "--framing-fprime-flight-frame-size", str(frame),
             "--files-chunk-size", str(chunk)], capture_output=True, text=True, timeout=30)
        assert check(FPRIME_FRAME, FPRIME_CHUNK).returncode == 0
        done = check(FPRIME_FRAME, FPRIME_CHUNK + 1)
        assert done.returncode != 0 and f"at most {FPRIME_CHUNK} bytes of file data" in done.stderr, done
        assert check(FPRIME_FRAME + 1, 100).returncode != 0
        ok(f"fprime: with --framing-fprime-flight-frame-size {FPRIME_FRAME} the chunk size {FPRIME_CHUNK} is accepted, {FPRIME_CHUNK + 1} refused")

    gds = Fgds(args, out, f"{out}/store", ["--files-chunk-size", str(on_ref)])
    ref = None
    try:
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(args.ref, proxy, out)
        scenario.wait_for("telemetry", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])
        packets = lambda: file_packets(proxy.uplink, args.framing, scenario.spacecraft_id(args.dictionary))

        def uplink(chunk, destination):
            return subprocess.run([args.fgds, "file-uplink", "--url", gds.root, "--file-uplink-chunk-size", str(chunk),
                                   "local/big.bin", destination], capture_output=True, text=True, cwd=out, timeout=90)

        # The largest size: by the option of the command line and by the option of `fgds run`.
        for destination, done in (("cli.bin", uplink(on_ref, "cli.bin")),
                                  ("run.bin", subprocess.run([args.fgds, "file-uplink", "--url", gds.root, "local/big.bin", "run.bin"],
                                                             capture_output=True, text=True, cwd=out, timeout=90))):
            assert done.returncode == 0, done
            assert read(f"{out}/{destination}") == content("big.bin", size), destination
        data = [p for p in packets() if p[2] == 1]
        assert len(data) == 8 and max(len(p) for p in data) == on_ref + 13, [len(p) for p in data]
        # One more: refused, nothing sent, no file on Ref, no transfer listed.
        before, listed = len(packets()), len(gds.get("/files/uplink")["files"])
        done = uplink(largest + 1, "over.bin")
        assert done.returncode != 0 and f"at most {largest} bytes of file data" in done.stdout + done.stderr, done
        try:
            request = urllib.request.Request(f"{gds.api}/files/uplink?name=over.bin&destination=over.bin&chunk={largest + 1}", b"x", method="POST")
            urllib.request.urlopen(request, timeout=20)
            raise AssertionError("the route accepted the chunk size")
        except urllib.error.HTTPError as error:
            assert error.code == 422 and f"at most {largest} bytes" in json.load(error)["error"]["message"]
        assert len(packets()) == before and len(gds.get("/files/uplink")["files"]) == listed
        assert not os.path.exists(f"{out}/over.bin")
        if args.framing == "fprime":
            # The limit is the framer's, for every packet: a raw packet of one byte more than the frame takes is not sent.
            sent = len(proxy.uplink)
            for length, accepted in ((2036, True), (2037, False)):
                done = subprocess.run([args.fgds, "send-raw", "--url", gds.root, "--hex-string", "00" * length],
                                      capture_output=True, text=True, timeout=30)
                assert (done.returncode == 0) == accepted, (length, done)
                assert accepted or "frameAccumulatorSize of ComFprime" in done.stdout + done.stderr, done
            scenario.wait_for("the raw packet on the link", lambda: len(proxy.uplink) >= sent + 2048)
            assert len(proxy.uplink) == sent + 2048, len(proxy.uplink) - sent
        gds.healthy(f"{out}/fgds.log")
        ok(f"{args.framing}: chunk size {on_ref} reaches Ref whole, {size} bytes in {on_ref + 13}-byte packets; "
           f"{largest + 1} is refused by fgds run, fgds check, fgds file-uplink and the route, with the limit in the message and nothing sent")
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)


if __name__ == "__main__":
    main()
