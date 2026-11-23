"""Records how argparse of `fprime-cli command-send` reads command lines: `table.json`.

Run with the Python of the fprime virtual environment (fprime-gds installed):
    python fixtures/command-line/generate.py
Each row: the tokens after `command-send`, then either `error` (the message after `error: `, exit code 2),
`help`, or the values argparse took.
"""
import contextlib
import io
import json
import pathlib
import random

from fprime_gds.executables.fprime_cli import create_parser

NAME = "CdhCore.cmdDisp.CMD_NO_OP_STRING"
POOL = [
    NAME, "x", "5", "1.5", "", " ", "- 5", "a b", "-", "--", "---", "-5", "-.5", "-5.", "-1.5", "-1e5", "-0x10", "-1_0",
    "-\u0665", "-\u0665.\u0665", "-5\n", "-inf", "-Infinity", "-NaN", "-nan", "-x", "-e", "-1-2", "-+1", "+1", "--inf",
    "--bogus", "--bogus=1", "-j", "-jj", "-ji5", "-j5", "-j=5", "-jl", "-jlX", "-jx", "-i", "-i5", "-i=5", "-i5.5", "-iabc",
    "-i\u0665", "-i 5", "-i-5", "-c", "-cX", "-s", "-sX", "-s=X", "-l", "-lX", "-l=X", "-d", "-dX", "-h", "--help", "--he",
    "--list", "--li", "--l", "--lo", "--logs", "--logs=X", "--log", "--log-d", "--log-level-gds", "--log-level-gds=INFO",
    "--log-level-gds=x", "--json", "--js", "--j", "--json=1", "--ids", "--ids=5", "--id", "--i", "--components", "--comp",
    "--search", "--se", "--search=-x", "--arguments", "--arg", "--a", "--arguments=", "--arguments=-inf", "--arg=-1e-400",
    "--arguments=a=b", "--dictionary", "--dict", "--d", "--di", "--de", "--zmq-transport", "--tts-port", "--tts-port=x",
    "--tts-port=5", "--tts", "--file-uplink-cooldown", "--file-uplink-cooldown=1e3", "--file-uplink-cooldown=x", "--no-zmq",
    "--no", "--no-zmq=1", "--packet", "--packet-spec", "--p", "--hash-file", "-V", "-t", "-n", "5", "abc", "-5", NAME,
]
FIELDS = ["command_name", "arguments", "is_printing_list", "ids", "components", "search", "json", "logs", "dictionary"]
DEFAULT_LOGS = create_parser().parse_args(["command-send"]).logs


def read(tokens):
    parser = create_parser()
    out, err = io.StringIO(), io.StringIO()
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            args = parser.parse_args(["command-send", *tokens])
    except SystemExit as stop:
        if stop.code == 0:
            return {"help": True}
        assert stop.code == 2, stop.code
        error = err.getvalue().split(": error: ", 1)[1].removesuffix("\n")
        if error.startswith("ambiguous option: "):
            # fprime-cli builds its parser from a set of parents: the order of the matches changes from run to run.
            start, matches = error.split(" could match ")
            error = start + " could match " + ", ".join(sorted(matches.split(", ")))
        return {"error": error}
    values = {field: getattr(args, field) for field in FIELDS}
    if values["logs"] == DEFAULT_LOGS:
        values["logs"] = None
    return {"values": values}


def main():
    chance = random.Random(7)
    rows = [[]] + [[token] for token in POOL] + [[NAME, "--arguments", token] for token in POOL]
    rows += [["--arguments", token, NAME] for token in POOL] + [[NAME, token, "--arguments", "1"] for token in POOL]
    rows += [[chance.choice(POOL) for _ in range(chance.randint(2, 6))] for _ in range(6000)]
    seen, table = set(), []
    for tokens in rows:
        if tuple(tokens) not in seen:
            seen.add(tuple(tokens))
            table.append({"tokens": tokens, **read(tokens)})
    path = pathlib.Path(__file__).with_name("table.json")
    path.write_text(json.dumps(table, ensure_ascii=True, indent=0) + "\n")
    print(f"{len(table)} rows: {sum('error' in row for row in table)} refused, {sum('help' in row for row in table)} help")


main()
