#!/usr/bin/env python3
"""Run the `Ref` integration tests of F Prime (`TestDeploymentsProject/Ref/test/int`, unchanged) against `fgds`.

Starts `fgds` and `Ref`, then pytest with the `fgds_client` plugin. Exit code of pytest.
"""
import argparse
from datetime import datetime
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from xml.etree import ElementTree

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tests" / "compare"))
import scenario  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("--fgds", required=True)
parser.add_argument("--ref", required=True)
parser.add_argument("--dictionary", required=True)
parser.add_argument("--out", required=True)
parser.add_argument("--tests", required=True, help="the test/int directory of Ref")
args, pytest_args = parser.parse_known_args()

out = os.path.abspath(args.out)
os.makedirs(out, exist_ok=True)
tests = f"{out}/int"
shutil.rmtree(tests, ignore_errors=True)
shutil.copytree(args.tests, tests)
http, link = scenario.free_port(), scenario.free_port()
url = f"http://127.0.0.1:{http}"
gds = scenario.start(
    [args.fgds, "run", "--logs", f"{out}/logs", "--dictionary", args.dictionary, "--bind", f"127.0.0.1:{http}", "--comm-tcp-address", f"127.0.0.1:{link}"],
    open(f"{out}/fgds.log", "w"),
)
ref = None
try:
    scenario.wait_for("fgds", lambda: scenario.socket.create_connection(("127.0.0.1", http)).close() or True)
    ref = scenario.start([os.path.abspath(args.ref), "-a", "127.0.0.1", "-p", str(link)], open(f"{out}/ref.log", "w"), cwd=out)
    def run_tests(names, more=()):
        return subprocess.run(
            [sys.executable, "-m", "pytest", "-p", "no:cacheprovider", "-p", "no:fprime_test_api", "-p", "fgds_client.pytest_plugin",
             "--rootdir", ".", "-v", "-rs", "--fgds-url", url, "--fgds-logs", f"{out}/logs",
             "--deployment-config", "int_config.json", "--dictionary", os.path.abspath(args.dictionary), *more,
             *names],
            cwd=tests,
            env={**os.environ, "PYTHONPATH": str(ROOT / "python"), "FGDS": os.path.abspath(args.fgds),
                 "PATH": os.path.dirname(sys.executable) + os.pathsep + os.environ["PATH"]},
        ).returncode

    def lost_by_ref(failed):
        """Events Ref printed during the failed tests and never wrote to the link: their times are not in the test log.

        Ref stamps events with the clock of the machine, so a test's lines in the log give its span of flight time.
        """
        printed = {(int(s), int(u)) for s, u in re.findall(r"^EVENT: \(\d+\) \(2:(\d+),(\d+)\)", open(f"{out}/ref.log").read(), re.M)}
        received, spans = set(), []
        for log in Path(f"{out}/logs").glob("test-*.log"):
            text = log.read_text()
            received |= {(int(s), int(u)) for s, u in re.findall(r"\(2\(0\)-(\d+):(\d+)\)", text)}
            starts = [(datetime.fromisoformat(stamp + "+00:00").timestamp(), name)
                      for stamp, name in re.findall(r"^(\S+) \[Test API\] \[STARTING CASE\] (\S+)", text, re.M)]
            ends = [start for start, _ in starts[1:]] + [float("inf")]
            spans += [(start, end) for (start, name), end in zip(starts, ends) if any(test.endswith(f"::{name}") for test in failed)]
        return sorted(t for t in printed - received if any(start <= t[0] + t[1] / 1e6 < end for start, end in spans))

    report = f"{out}/results.xml"
    code = run_tests(sorted(name for name in os.listdir(tests) if name.endswith(".py")), [*pytest_args, "--junitxml", report])
    failed = [f"{case.get('classname').replace('.', '/')}.py::{case.get('name')}"
              for case in ElementTree.parse(report).iter("testcase") if case.find("failure") is not None or case.find("error") is not None]
    # Ref drops events of a burst now and then (with the Python GDS too: test_seqgen_complex_args failed
    # in 1 of 3 runs of the verifier). A test that failed is run once more, alone, only when Ref printed
    # events that it never wrote to the link; any other failure stands.
    if code and failed and (lost := lost_by_ref(failed)):
        print(f"ref_int: Ref printed {len(lost)} event(s) that it did not send (first at {lost[0][0]}:{lost[0][1]}); "
              f"running again: {', '.join(failed)}", flush=True)
        code = run_tests(failed)
finally:
    for process in (ref, gds):
        if process:
            scenario.stop(process)
sys.exit(code)
