#!/usr/bin/env python3
"""Every plugin option of `fgds run`, with values it cannot honour: `fgds check` and `fgds run` refuse each with
the same message, and `fgds run` has opened nothing. An option that is not in the table fails the check."""
import argparse
import os
import re
import shutil
import socket
import subprocess

# The values refused, by option. A new plugin option must be added here with the values it refuses.
INVALID = {
    "storage-memory-max-records": ["x", "-1", "1.5", ""],
    "comm-tcp-address": ["x", ":5", "host:", "host:70000", "host:x", ""],
    "comm-tcp-keepalive-interval": ["x", "-1", "nan", "inf", ""],
    "comm-uart-device": [""],
    "comm-uart-baud": ["x", "7", "0", "-9600", "9600.5", ""],
    "comm-udp-address": ["x", ":5", "host:0", "host:70000", ""],
    "comm-udp-recv-port": ["x", "0", "70000", "-1", ""],
    "framing-ccsds-scid": ["x", "0", "1024", "-1", "0x400", ""],
    "framing-ccsds-vcid": ["x", "64", "99", "-1", ""],
    "framing-ccsds-frame-size": ["x", "0", "8", "-1", "1.5", ""],
    "files-directory": ["", "/etc/passwd", "/etc/passwd/under"],
    # 998: one more than the CCSDS framing carries (tests/files/chunk.py has both framings against Ref).
    "files-chunk-size": ["x", "0", "70000", "-1", "1.5", "998", ""],
    "files-cooldown": ["x", "-1", "nan", "inf", ""],
    "files-timeout": ["x", "0", "-1", "nan", ""],
    "files-max-upload-bytes": ["x", "-1", "1.5", "4294967296", ""],
    "data-products-dir": ["", "/etc/passwd", "/etc/passwd/under"],
    "data-products-settle-ms": ["x", "-1", "1.5", ""],
    "data-products-max-bytes": ["x", "-1", "1.5", ""],
}
# Values that must stay accepted, so that the refusals above are not a refusal of everything.
VALID = {
    "storage-memory-max-records": "0", "comm-tcp-address": "127.0.0.1:0", "comm-tcp-keepalive-interval": "0",
    "comm-uart-device": "/dev/ttyUSB7", "comm-uart-baud": "9600", "comm-udp-address": "localhost:50000",
    "comm-udp-recv-port": "50001", "framing-ccsds-scid": "0x44", "framing-ccsds-vcid": "0",
    "framing-ccsds-frame-size": "1024", "files-directory": "/tmp/fgds-not-made/files", "files-chunk-size": "997",
    "files-cooldown": "0", "files-timeout": "0.5", "files-max-upload-bytes": "0",
    "data-products-dir": "/tmp/fgds-not-made/dp", "data-products-settle-ms": "0", "data-products-max-bytes": "0",
}


def main():
    parser = argparse.ArgumentParser()
    for name in ("fgds", "dictionary", "out"):
        parser.add_argument(f"--{name}", required=True)
    args = parser.parse_args()
    shutil.rmtree(args.out, ignore_errors=True)
    os.makedirs(args.out)

    usage = subprocess.run([args.fgds, "run", "--help"], capture_output=True, text=True, check=True).stdout
    options = re.findall(r"^\s+--([a-z0-9-]+)", usage, re.M)
    # The plugin options are those after the options of fgds itself, which end with --bind.
    plugin = options[options.index("bind") + 1:]
    plugin = [name for name in plugin if name not in ("help", "version")]
    assert sorted(plugin) == sorted(INVALID) == sorted(VALID), \
        f"plugin options and this table differ: {sorted(set(plugin) ^ set(INVALID) | set(plugin) ^ set(VALID))}"

    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    common = ["--dictionary", args.dictionary, "--bind", f"127.0.0.1:{port}", "--comm-tcp-address", "127.0.0.1:0"]

    def run(subcommand, option, value, logs):
        command = [args.fgds, subcommand, *(common[:4] if option == "comm-tcp-address" else common), "--logs", logs]
        return subprocess.run([*command, f"--{option}={value}"], capture_output=True, text=True, timeout=20)

    count = 0
    for option in plugin:
        logs = f"{args.out}/logs"
        passed = run("check", option, VALID[option], logs)
        assert passed.returncode == 0, (option, VALID[option], passed.stderr)
        for value in INVALID[option]:
            check = run("check", option, value, logs)
            started = run("run", option, value, logs)
            for done in (check, started):
                assert done.returncode != 0 and done.stderr.startswith("error: "), (option, value, done)
            assert check.stderr == started.stderr, (option, value, check.stderr, started.stderr)
            # The message names the option, by its name in the plugin.
            plugin_name = max((p for p in ("storage-memory", "comm-tcp", "comm-uart", "comm-udp", "framing-ccsds", "files", "data-products")
                               if option.startswith(p + "-")), key=len)
            assert plugin_name in check.stderr and option[len(plugin_name) + 1:].replace("-", " ") in check.stderr.replace("-", " "), \
                (option, value, check.stderr)
            assert not os.path.exists(logs), f"fgds run --{option}={value} opened its storage before refusing"
            count += 1
        assert not os.path.exists("/tmp/fgds-not-made")
    print(f"  ok: {len(plugin)} plugin options, {count} values that cannot be honoured: fgds check and fgds run refuse each with the "
          "same message and fgds run opens nothing; one good value of each passes fgds check")


if __name__ == "__main__":
    main()
