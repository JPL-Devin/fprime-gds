#!/usr/bin/env python3
"""Run the `Ref` integration tests of F Prime (copies in `python/tests/ref_int`) against `fgds`.

Starts `fgds` and `Ref`, then pytest with the `fgds_client` plugin. Exit code of pytest.
"""
import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tests" / "compare"))
import scenario  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--fgds", required=True)
parser.add_argument("--ref", required=True)
parser.add_argument("--dictionary", required=True)
parser.add_argument("--out", required=True)
args, pytest_args = parser.parse_known_args()

out = os.path.abspath(args.out)
os.makedirs(out, exist_ok=True)
http, link = scenario.free_port(), scenario.free_port()
url = f"http://127.0.0.1:{http}"
gds = scenario.start(
    [args.fgds, "run", "--framing", "fprime", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}", "--comm-tcp-address", f"127.0.0.1:{link}"],
    open(f"{out}/fgds.log", "w"),
)
ref = None
try:
    scenario.wait_for("fgds", lambda: scenario.socket.create_connection(("127.0.0.1", http)).close() or True)
    ref = scenario.start([os.path.abspath(args.ref), "-a", "127.0.0.1", "-p", str(link)], open(f"{out}/ref.log", "w"), cwd=out)
    code = subprocess.run(
        [sys.executable, "-m", "pytest", "-p", "no:cacheprovider", "-p", "no:fprime_test_api", "-p", "fgds_client.pytest_plugin",
         "-c", "pytest.ini", "--rootdir", ".", "-v", "-rs", "--fgds-url", url, "--fgds-logs", f"{out}/logs",
         "--dictionary", os.path.abspath(args.dictionary), *pytest_args, "."],
        cwd=ROOT / "python" / "tests" / "ref_int",
        env={**os.environ, "PYTHONPATH": str(ROOT / "python")},
    ).returncode
finally:
    for process in (ref, gds):
        if process:
            scenario.stop(process)
sys.exit(code)
