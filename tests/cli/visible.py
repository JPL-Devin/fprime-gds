#!/usr/bin/env python3
"""No text output of fgds carries a character a terminal acts on (deviations.md, "Control characters").

A dictionary whose texts hold escape sequences, line ends and direction marks, an event of Ref that carries
them in its string, and operator text that holds them (file names, arguments, option values) go through
every subcommand that prints text. Each output must hold no byte below 0x20 but the line end, no 0x7F, no
C1 character and none of the characters listed in `HIDDEN`; the JSON outputs must hold the texts exactly."""
import argparse
import base64
import json
import os
import shutil
import subprocess
import sys
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "files"))
import scenario  # noqa: E402
from check import Fgds, ok, prepare  # noqa: E402

HIDDEN = set("\u2028\u2029\u061c\u200e\u200f\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069\u200b\u200c\u200d\u2060\ufeff")
# Retitle the terminal, clear it, colour, a line that is not one, DEL, C1, and the characters of HIDDEN.
HOSTILE = "\x1b]0;T\x07\x1b[2J\x1b[31m\r\nfake\t\x7f\u009b2J\\" + "".join(sorted(HIDDEN))
# The string of the event of Ref: 40 bytes at most.
STRINGS = ["a\x1b]0;T\x07b\x1b[2Jc\x1b[31mR\r\nfake\t\x7f\\n", "\u009b2J\u2028\u2029\u061c\u200e\u202e\u2066\u200b\u2060\ufeff"]
NO_OP_STRING, RECEIVED, DISPATCHED = "CdhCore.cmdDisp.CMD_NO_OP_STRING", "CdhCore.cmdDisp.NoOpStringReceived", "CdhCore.cmdDisp.CommandsDispatched"


def clean(what, output):
    """`output` (bytes) is text that a terminal shows and does not act on."""
    text = output.decode("utf-8")
    # The one control character fgds itself prints: the tab that begins an argument line of a command
    # definition, as `fprime-cli commands` (a tab of the data is printed `\\t`).
    text = "\n".join(line[1:] if line.startswith("\t") else line for line in text.split("\n"))
    bad = sorted({c for c in text if (c < " " and c != "\n") or "\x7f" <= c <= "\x9f" or c in HIDDEN})
    assert not bad, (what, [hex(ord(c)) for c in bad], output[:2000])
    assert not any(line.startswith("fake") for line in text.split("\n")), (what, "a line of the data reads as a line of fgds", output[:2000])
    return text


def main():
    parser = argparse.ArgumentParser()
    for name in ("fgds", "ref", "dictionary", "out"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--framing", default="ccsds")
    args = parser.parse_args()
    out = os.path.abspath(f"{args.out}/visible")
    prepare(out)
    # Operator text: a directory and files named with the hostile text (a file name cannot hold `/` or NUL).
    hostile_dir = f"{out}/d{HOSTILE}"
    os.makedirs(hostile_dir)
    with open(args.dictionary) as file:
        dictionary = json.load(file)
    find = lambda section, name: next(item for item in dictionary[section] if item["name"] == name)
    command, event, channel = find("commands", NO_OP_STRING), find("events", RECEIVED), find("telemetryChannels", DISPATCHED)
    command["annotation"] = command["formalParams"][0]["annotation"] = f"No-op {HOSTILE}"
    event["format"], event["annotation"] = "Received \x1b[31m\u202ea NO-OP\r\nfake string={}", f"event {HOSTILE}"
    channel["format"], channel["annotation"] = "{} \x1b[2J\r\nfake", f"channel {HOSTILE}"
    args.dictionary = f"{hostile_dir}/dictionary.json"
    with open(args.dictionary, "w") as file:
        json.dump(dictionary, file)
    for name, data in (("file.bin", b"x" * 300), ("no-op.bin", b"\x00"), ("empty.json", b"{}"),
                       ("seq.seq", f'R00:00:00 {NO_OP_STRING} "x"\nR00:00:00 No.Such{HOSTILE} 1\n'.replace("\r\n", " ").encode()),
                       ("prm.json", json.dumps({f"comp{HOSTILE}": {f"p{HOSTILE}": HOSTILE}}).encode()),
                       ("Dp_1_2_3.fdp", HOSTILE.encode() * 3)):
        with open(f"{hostile_dir}/{name}", "wb") as file:
            file.write(data)

    count = 0

    def run(what, *command, shows=(), exact=False, **options):
        nonlocal count
        done = subprocess.run([args.fgds, *command], capture_output=True, timeout=120, cwd=out, **options)
        if exact:
            return done.stdout.decode()
        text = clean((what, "standard output"), done.stdout) + clean((what, "standard error"), done.stderr)
        for shown in shows:
            assert shown in text, (what, shown, text[:3000])
        count += 1
        return text

    offline = ["--dictionary", args.dictionary]
    run("check", "check", *offline, "--logs", f"{out}/unused", shows=["\\x1b[2J", "\\r\\nfake"])
    run("check, refused", "check", "--dictionary", f"{hostile_dir}/absent.json", shows=["\\x1b"])
    run("unknown subcommand", f"sub{HOSTILE}", shows=["\\x1b"])
    run("unknown option", "events", f"--opt{HOSTILE}", shows=["\\x1b"])
    run("option value", "check", *offline, f"--files-chunk-size={HOSTILE}", shows=["\\x1b"])
    run("seqgen", "seqgen", *offline, f"{hostile_dir}/seq.seq", f"{out}/seq.bin", "--all-errors", shows=["\\x1b"])
    run("seqgen, no file", "seqgen", *offline, f"{hostile_dir}/absent.seq", shows=["\\x1b"])
    run("seqgen, time base", "seqgen", *offline, f"{hostile_dir}/seq.seq", "-t", HOSTILE, shows=["\\x1b"])
    for kind in ("dat", "seq"):
        run(f"prm-write {kind}", "prm-write", *offline, kind, f"{hostile_dir}/prm.json", "-o", f"{hostile_dir}/prm.{kind}", shows=["\\x1b"])
        run(f"prm-write {kind}, no file", "prm-write", *offline, kind, f"{hostile_dir}/absent.json", shows=["\\x1b"])
    run("prm-decode", "prm-decode", *offline, f"{hostile_dir}/file.bin", shows=["\\x1b"])
    run("prm-decode, no file", "prm-decode", *offline, f"{hostile_dir}/absent.dat", shows=["\\x1b"])
    run("merge-dictionary", "merge-dictionary", args.dictionary, args.dictionary, "--output", f"{hostile_dir}/merged.json")
    run("merge-dictionary, no file", "merge-dictionary", args.dictionary, f"{hostile_dir}/absent.json", shows=["\\x1b"])
    run("merge-dictionary, no dictionary", "merge-dictionary", f"{hostile_dir}/prm.json", f"{hostile_dir}/empty.json", shows=["\\x1b"])
    run("dp decode", "dp", "decode", *offline, f"{hostile_dir}/Dp_1_2_3.fdp", shows=["\\x1b"])
    run("dp decode, no file", "dp", "decode", *offline, f"{hostile_dir}/absent.fdp", shows=["\\x1b"])
    run("dp decode, directory", "dp", "decode", *offline, hostile_dir, shows=["\\x1b"])
    run("dp validate", "dp", "validate", f"{hostile_dir}/Dp_1_2_3.fdp", "--verbose", shows=["\\x1b"])
    run("dp validate, dictionary", "dp", "validate", f"{hostile_dir}/Dp_1_2_3.fdp", "--dictionary", args.dictionary, shows=["\\x1b"])
    run("dp, unknown", "dp", f"x{HOSTILE}")

    gds = Fgds(args, out, f"{out}/store")
    ref = None
    try:
        proxy = scenario.Proxy(gds.link_port)
        ref = scenario.start_ref(args.ref, proxy, out)
        scenario.wait_for("telemetry", lambda: gds.get("/records?kind=fprime.channel&limit=1")["records"])
        url = ["--url", gds.root]
        for string in STRINGS:
            run("command-send", "command-send", NO_OP_STRING, "--arguments", string, *url)
        received = lambda: [r for r in gds.records("fprime.event") if r["body"]["name"] == RECEIVED]
        scenario.wait_for("the events of the strings", lambda: len(received()) == len(STRINGS))
        scenario.wait_for("the channel", lambda: [r for r in gds.records("fprime.channel") if r["body"]["name"] == DISPATCHED])
        # Storage is exact.
        assert [r["body"]["args"]["message"] for r in received()] == STRINGS, received()

        history = ["-H", "--no-follow", *url]
        text = run("events", "events", *history, shows=["Received \\x1b[31m\\u{202e}a NO-OP\\r\\nfake string=a\\x1b]0;T\\x07b\\x1b[2Jc\\x1b[31mR\\r\\nfake\\t\\x7f\\\\n",
                                                        "\\u{9b}2J\\u{2028}\\u{2029}\\u{61c}\\u{200e}\\u{202e}\\u{2066}\\u{200b}\\u{2060}\\u{feff}"])
        assert len([line for line in text.split("\n") if RECEIVED in line]) == len(STRINGS), text
        run("channels", "channels", *history, shows=[" \\x1b[2J\\r\\nfake"])
        run("events --list", "events", "--list", *url)
        run("channels --list", "channels", "--list", *url)
        run("commands", "commands", *url, shows=["No-op \\x1b]0;T\\x07\\x1b[2J"])
        # JSON is exact: the strings of the events, the format of the channel, the annotation of the command.
        events = [json.loads(line) for line in run("events -j", "events", "-j", *history, exact=True).splitlines()]
        assert [e["body"]["display_text"] for e in events if e["body"]["name"] == RECEIVED] == [event["format"].format(s) for s in STRINGS], events
        channels = [json.loads(line) for line in run("channels -j", "channels", "-j", *history, exact=True).splitlines()]
        assert any(c["body"]["display_text"].endswith(" \x1b[2J\r\nfake") for c in channels if c["body"]["name"] == DISPATCHED), channels[:3]
        commands = [json.loads(line) for line in run("commands -j", "commands", "-j", *url, exact=True).splitlines()]
        assert [c for c in commands if c["name"] == NO_OP_STRING][0]["annotation"] == command["annotation"], commands[:3]
        # --search reads the text as decoded, not as printed.
        found = run("events --search, decoded", "events", *history, "--search", "c\x1b[31mR\r\nfake")
        assert len([line for line in found.split("\n") if RECEIVED in line]) == 1, found
        assert RECEIVED not in run("events --search, printed", "events", *history, "--search", "c\\x1b[31mR")

        run("command-send, unknown", "command-send", f"No.Such{HOSTILE}", *url, shows=["\\x1b"])
        run("command-send, arguments", "command-send", NO_OP_STRING, "--arguments", "x", HOSTILE, *url, shows=["\\x1b"])
        run("command-send, dry", "command-send", NO_OP_STRING, "--arguments", HOSTILE[:20], "--dry-run", *url)
        run("command-send, numeric", "command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", HOSTILE, "1", "2", *url, shows=["u001b"])
        run("send-raw", "send-raw", "--bin-path", f"{hostile_dir}/absent.bin", *url, shows=["\\x1b"])
        run("send-raw, hex", "send-raw", "--hex-string", HOSTILE, *url, shows=["u{1b}"])
        run("send-raw, sent", "send-raw", "--bin-path", f"{hostile_dir}/no-op.bin", "--verbose", *url)
        # A source path with a control character is refused; one with the other characters is sent.
        marked = f"{out}/m\\\u202e\u200b"
        os.makedirs(marked)
        shutil.copy(f"{hostile_dir}/file.bin", f"{marked}/file.bin")
        run("file-uplink", "file-uplink", f"{marked}/file.bin", "up.bin", *url, shows=["Queued '", "m\\\\\\u{202e}\\u{200b}/file.bin", "Flight software confirmed receipt"])
        run("file-uplink, source", "file-uplink", f"{hostile_dir}/file.bin", "up.bin", *url)
        run("file-uplink, no file", "file-uplink", f"{hostile_dir}/absent.bin", *url, shows=["\\x1b"])
        run("file-uplink, destination", "file-uplink", f"{hostile_dir}/file.bin", f"up{HOSTILE}", *url)
        run("file-uplink, non-ASCII", "file-uplink", f"{marked}/file.bin", "\u00e9\u200b.bin", *url, shows=["\\u{200b}"])
        run("files upload", "files", "upload", f"{marked}/file.bin", "-d", "up2.bin", *url, shows=["\\u{202e}"])
        for sub in ("queue", "list", "pause", "resume"):
            run(f"files {sub}", "files", sub, *url)
        run("files cancel", "files", "cancel", HOSTILE, *url)
        run("files download", "files", "download", HOSTILE, *url)

        # A string of the flight software that is not UTF-8: the command packet of `AAAA` with those four
        # bytes replaced. The event is shown with U+FFFD for each byte that is no character, the record keeps
        # the bytes (`raw`), nothing is dropped.
        packet = run("command-send, packet", "command-send", NO_OP_STRING, "--arguments", "AAAA", "--dry-run", *url, exact=True).strip()
        assert packet.count("41414141") == 1, packet
        run("send-raw, not UTF-8", "send-raw", "--hex-string", packet.replace("41414141", "fffe8042"), *url)
        scenario.wait_for("the event of the string that is not UTF-8", lambda: len(received()) == len(STRINGS) + 1)
        record = received()[-1]
        assert record["body"]["args"]["message"] == "\ufffd\ufffd\ufffdB", record
        assert bytes.fromhex("fffe8042") in base64.b64decode(record["raw"]), record
        run("events, not UTF-8", "events", *history, shows=["string=\ufffd\ufffd\ufffdB"])

        # The text logs of the page: one line per record, as the standard output.
        for log in gds.get("/logs"):
            name = urllib.request.quote(log["name"], safe="")
            with gds.request(f"/logs/{name}?format=text&limit=10000") as response:
                lines = clean(("log", log["name"]), response.read())
            if log["name"] == "fprime.event":
                assert "string=a\\x1b]0;T\\x07b\\x1b[2Jc\\x1b[31mR\\r\\nfake\\t\\x7f\\\\n" in lines, lines[-3000:]
            count += 1
        gds.healthy(f"{out}/fgds.log")
    finally:
        for process in (ref, gds.process):
            if process:
                scenario.stop(process)
    # The log lines of `fgds run` on its standard error. None of them holds flight text (the run above, with
    # the events); the one that holds operator text is the path of the database, here under the hostile directory.
    with open(f"{out}/fgds.log", "rb") as file:
        clean("fgds run, standard error", file.read())
    port = scenario.free_port()
    with open(f"{out}/run.log", "w") as log:
        process = scenario.start([args.fgds, "run", "--dictionary", args.dictionary, "--logs", f"{hostile_dir}/logs", "--bind", f"127.0.0.1:{port}",
                                  "--comm-tcp-address", f"127.0.0.1:{scenario.free_port()}"], log)
        try:
            scenario.wait_for("fgds", lambda: urllib.request.urlopen(f"http://127.0.0.1:{port}/api/v1/version", timeout=5))
        finally:
            scenario.stop(process)
    with open(f"{out}/run.log", "rb") as file:
        log = clean("fgds run, standard error", file.read())
    assert "sqlite storage open" in log and "\\x1b]0;T\\x07\\x1b[2J" in log and "\x1b" not in log, log[-3000:]
    shutil.rmtree(hostile_dir)
    ok(f"{count + 1} text outputs with a hostile dictionary, hostile event strings and hostile operator text hold no control character, "
       "direction mark or character of no width; the JSON outputs and the stored records hold the texts exactly; --search reads the decoded text")


if __name__ == "__main__":
    main()
