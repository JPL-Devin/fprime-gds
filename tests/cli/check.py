#!/usr/bin/env python3
"""The subcommands of `fgds` that talk to a running GDS, against Ref: what they print, where, and how they end."""
import argparse
import json
import os
import shutil
import subprocess
import sys
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compare"))
import scenario  # noqa: E402

NO_OP = "CdhCore.cmdDisp.CMD_NO_OP"


def ok(text):
    print(f"  ok: {text}")


def main():
    parser = argparse.ArgumentParser()
    for name in ("fgds", "ref", "dictionary", "out"):
        parser.add_argument(f"--{name}", required=True)
    args = parser.parse_args()
    out = args.out
    shutil.rmtree(out, ignore_errors=True)
    os.makedirs(out)
    http, link = scenario.free_port(), scenario.free_port()
    url = f"http://127.0.0.1:{http}"
    gds = scenario.start(
        [args.fgds, "run", "--logs", f"{out}/logs", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}",
         "--comm-tcp-address", f"127.0.0.1:{link}", "--files-directory", f"{out}/store"], open(f"{out}/gds.log", "w"))
    ref = None

    def get(path):
        with urllib.request.urlopen(f"{url}/api/v1{path}", timeout=10) as response:
            return json.load(response)

    def cli(subcommand, *options, timeout=60):
        done = subprocess.run([args.fgds, subcommand, "--url", url, *options], capture_output=True, text=True, cwd=out, timeout=timeout)
        return done.returncode, done.stdout, done.stderr

    try:
        scenario.wait_for("fgds", lambda: get("/version"))
        ref = scenario.start_ref(args.ref, scenario.Proxy(link), out)
        scenario.wait_for("telemetry", lambda: get("/records?kind=fprime.channel&limit=1")["records"])
        sent = lambda: len(get("/records?kind=fprime.command&limit=1000")["records"])

        before = sent()
        code, _, stderr = cli("command-send", NO_OP)
        assert (code, stderr) == (0, ""), (code, stderr)
        scenario.wait_for("the command record", lambda: sent() == before + 1)
        code, stdout, stderr = cli("command-send", "CdhCore.cmdDisp.NOT_A_COMMAND")
        assert code == 1 and stdout == "" and "NOT_A_COMMAND" in stderr, (code, stdout, stderr)
        # A finite literal no F32 holds, and too few arguments: refused, nothing sent.
        for arguments in (["1", "1e400", "1"], ["1"]):
            code, _, _ = cli("command-send", "CdhCore.cmdDisp.CMD_TEST_CMD_1", "--arguments", *arguments)
            assert code != 0, arguments
        dispatched = lambda: [r for r in get("/records?kind=fprime.event&limit=1000")["records"] if "OpCodeDispatched" in r["body"]["name"]]
        scenario.wait_for("the command at Ref", lambda: len(dispatched()) == 1)
        ok("command-send: a command is sent with exit code 0; an unknown command, a literal beyond the type and a "
           "missing argument end with a non-zero exit code (the unknown command with a message on standard error) and Ref dispatched the one command only")

        code, stdout, _ = cli("events", "--list")
        assert code == 0 and "CdhCore.cmdDisp.NoOpReceived" in stdout, stdout[:300]
        code, stdout, _ = cli("channels", "--list")
        assert code == 0 and "CdhCore.cmdDisp.CommandsDispatched" in stdout, stdout[:300]
        code, stdout, _ = cli("commands")
        assert code == 0 and NO_OP in stdout, stdout[:300]
        code, stdout, _ = cli("channels", "-t", "5")
        assert code == 0 and stdout.strip(), "a channel arrives within 5 s"
        code, stdout, _ = cli("events", "-t", "1", "--components", "no.such.component")
        assert code == 1 and stdout == "", (code, stdout)
        ok("events, channels, commands: the definitions are listed; a channel that arrives is printed with exit code 0; "
           "no event within the time-out is exit code 1")

        with open(f"{out}/up.bin", "wb") as file:
            file.write(bytes(range(256)) * 8)
        code, stdout, _ = cli("file-uplink", "up.bin", "cli-up.bin")
        assert code == 0 and "Flight software confirmed receipt" in stdout, stdout
        assert open(f"{out}/cli-up.bin", "rb").read() == bytes(range(256)) * 8
        code, stdout, _ = cli("file-uplink", "up.bin")
        assert code == 1 and "no destination on the spacecraft" in stdout, stdout
        code, stdout, _ = cli("file-uplink", "up.bin", "/nonexistent-dir/sub/up.bin")
        assert code == 1 and "failed: " in stdout and "FileOpenError" in stdout, stdout
        states = {f["destination"]: (f["state"], f["reason"]) for f in get("/files/uplink")["files"]}
        assert states["cli-up.bin"] == ("finished", None), states
        assert states["/nonexistent-dir/sub/up.bin"][0] == "failed" and "FileOpenError" in states["/nonexistent-dir/sub/up.bin"][1], states
        ok("file-uplink: a file Ref received ends as finished with exit code 0; one Ref could not open ends as failed "
           "with the event of Ref as the reason; without a destination nothing is sent")
        assert gds.poll() is None and ref.poll() is None
    finally:
        for process in (ref, gds):
            if process:
                scenario.stop(process)
    print("cli: all passed")


if __name__ == "__main__":
    main()
