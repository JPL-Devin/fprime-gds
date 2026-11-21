#!/usr/bin/env python3
"""Compare the sequence compiler of fgds with the Python compiler on generated sequences.

Each sequence is compiled by `seqgen.generateSequence` of the Python GDS and by `fgds seqgen`.
Both must accept it with the same bytes, or both must reject it; when the Python compiler
rejects with a message of its own (not a crash), the message of fgds must be the same text.
Two known differences are allowed (docs/design/deviations.md): when reporting every error,
fgds also reports the statement after one with a bad time tag, and a value out of the range
of F32, which the Python compiler skips or crashes on; and the error of a quoted array or
object inside an array names its line, where the Python compiler always says line 1.

    python differential.py --fgds target/debug/fgds --fixtures fixtures --out DIR

Run with the Python of the F Prime virtual environment (fprime_gds installed).
"""
import argparse
import contextlib
import io
import random
import re
import subprocess
import sys
from pathlib import Path

from fprime_gds.common.tools import seqgen

# Lines that exercise the lexer, the time tags and the argument conversions.
LINES = """
R00:00:00 cmdDisp.CMD_NO_OP
R00:00:00 cmdDisp.CMD_NO_OP ; comment
R00:00:01.5 cmdDisp.CMD_NO_OP
R00:00:01.000001 cmdDisp.CMD_NO_OP
R23:59:59.999999 cmdDisp.CMD_NO_OP
R24:00:00 cmdDisp.CMD_NO_OP
R00:60:00 cmdDisp.CMD_NO_OP
R00:00:60 cmdDisp.CMD_NO_OP
R00:00:62 cmdDisp.CMD_NO_OP
R00:00:62.5 cmdDisp.CMD_NO_OP
R00:00:00.1234567 cmdDisp.CMD_NO_OP
R0:0:0 cmdDisp.CMD_NO_OP
A2015-075T22:32:40.123 cmdDisp.CMD_NO_OP
A2023-366T00:00:00 cmdDisp.CMD_NO_OP
A2024-366T00:00:00 cmdDisp.CMD_NO_OP
A2023-367T00:00:00 cmdDisp.CMD_NO_OP
A2023-000T00:00:00 cmdDisp.CMD_NO_OP
A0000-001T00:00:00 cmdDisp.CMD_NO_OP
A1969-365T23:59:59 cmdDisp.CMD_NO_OP
A2106-038T06:28:15 cmdDisp.CMD_NO_OP
A2106-038T06:28:16 cmdDisp.CMD_NO_OP
A1970-001T00:00:00.000001 cmdDisp.CMD_NO_OP
R 00:00:00 cmdDisp.CMD_NO_OP
r00:00:00 cmdDisp.CMD_NO_OP
R00:00:00 cmdDisp.CMD_UNKNOWN 1, 2
R00:00:00 R
R00:00:00 A 1
R00:00:00 $cmdDisp.CMD_NO_OP
R00:00:00 cmdDisp.CMD_NO_OP 1 2
R00:00:00 cmdDisp.CMD_NO_OP 1, 2, "ignored"
R00:00:00 cmdDisp.CMD_STRING "hello"
R00:00:00 cmdDisp.CMD_STRING 'single'
R00:00:00 cmdDisp.CMD_STRING "with \\"escape\\" and \\\\"
R00:00:00 cmdDisp.CMD_STRING "é unicode ☃"
R00:00:00 cmdDisp.CMD_STRING ""
R00:00:00 cmdDisp.CMD_STRING \"\"\"triple\"\"\"
R00:00:00 cmdDisp.CMD_STRING "unterminated
R00:00:00 cmdDisp.CMD_STRING "01234567890123456789012345678901234567890123456789"
R00:00:00 cmdDisp.CMD_STRING name.with.dots
R00:00:00 cmdDisp.CMD_STRING 12
R00:00:00 cmdDisp.CMD_STRING 1.5
R00:00:00 cmdDisp.CMD_STRING true
R00:00:00 cmdDisp.CMD_STRING [1, 2.0, "x", true, [3], {a: 1}]
R00:00:00 cmdDisp.CMD_STRING {a: 1, b: "two", a: 3}
R00:00:00 cmdDisp.CMD_STRING ["[1, 2]", "{x"]
R00:00:00 cmdDisp.CMD_STRING \\
  "continued"
R00:00:00 cmdDisp.CMD_INT 1
R00:00:00 cmdDisp.CMD_FLOAT 2.5
R00:00:00 cmdDisp.CMD_INT 3
R00:00:00 cmdDisp.CMD_FLOAT 1
R00:00:00 cmdDisp.CMD_INT 0x10
R00:00:00 cmdDisp.CMD_FLOAT 1e3
R00:00:00 cmdDisp.CMD_INT -0
R00:00:00 cmdDisp.CMD_FLOAT 0x10
R00:00:00 cmdDisp.CMD_INT 0X1F
R00:00:00 cmdDisp.CMD_FLOAT .5
R00:00:00 cmdDisp.CMD_INT +7
R00:00:00 cmdDisp.CMD_FLOAT 0X1F
R00:00:00 cmdDisp.CMD_INT 1_000
R00:00:00 cmdDisp.CMD_FLOAT 1_0.0_1
R00:00:00 cmdDisp.CMD_INT 007
R00:00:00 cmdDisp.CMD_FLOAT 1_000
R00:00:00 cmdDisp.CMD_INT "12"
R00:00:00 cmdDisp.CMD_FLOAT "1.5"
R00:00:00 cmdDisp.CMD_INT "0x7f"
R00:00:00 cmdDisp.CMD_FLOAT "12"
R00:00:00 cmdDisp.CMD_INT "0o17"
R00:00:00 cmdDisp.CMD_FLOAT "nan"
R00:00:00 cmdDisp.CMD_INT "0b11"
R00:00:00 cmdDisp.CMD_FLOAT "0o17"
R00:00:00 cmdDisp.CMD_INT " 12 "
R00:00:00 cmdDisp.CMD_FLOAT " inf "
R00:00:00 cmdDisp.CMD_INT "1_0"
R00:00:00 cmdDisp.CMD_FLOAT " 12 "
R00:00:00 cmdDisp.CMD_INT "012"
R00:00:00 cmdDisp.CMD_FLOAT "1e400"
R00:00:00 cmdDisp.CMD_INT "abc"
R00:00:00 cmdDisp.CMD_FLOAT "012"
R00:00:00 cmdDisp.CMD_INT 2147483648
R00:00:00 cmdDisp.CMD_FLOAT 1e39
R00:00:00 cmdDisp.CMD_INT 256
R00:00:00 cmdDisp.CMD_FLOAT 2147483648
R00:00:00 cmdDisp.CMD_INT -2147483649
R00:00:00 cmdDisp.CMD_FLOAT -1e39
R00:00:00 cmdDisp.CMD_INT -1
R00:00:00 cmdDisp.CMD_FLOAT -2147483649
R00:00:00 cmdDisp.CMD_INT 1.9
R00:00:00 cmdDisp.CMD_FLOAT 3
R00:00:00 cmdDisp.CMD_INT 2.999
R00:00:00 cmdDisp.CMD_FLOAT 1.9
R00:00:00 cmdDisp.CMD_INT 1e30
R00:00:00 cmdDisp.CMD_FLOAT 123456789012345678901234567890
R00:00:00 cmdDisp.CMD_INT 1e2
R00:00:00 cmdDisp.CMD_FLOAT 1e30
R00:00:00 cmdDisp.CMD_INT true
R00:00:00 cmdDisp.CMD_FLOAT false
R00:00:00 cmdDisp.CMD_INT True
R00:00:00 cmdDisp.CMD_FLOAT true
R00:00:00 cmdDisp.CMD_INT name
R00:00:00 cmdDisp.CMD_FLOAT [1]
R00:00:00 cmdDisp.CMD_INT {a: 1}
R00:00:00 cmdDisp.CMD_FLOAT name
R00:00:00 cmdDisp.CMD_INT 1e999
R00:00:00 cmdDisp.CMD_FLOAT 1e999
R00:00:00 cmdDisp.CMD_INT 0
R00:00:00 cmdDisp.CMD_FLOAT 1e999
R00:00:00 cmdDisp.CMD_INT 0x
R00:00:00 cmdDisp.CMD_FLOAT 1
R00:00:00 cmdDisp.CMD_INT 2
R00:00:00 cmdDisp.CMD_FLOAT 0x
R00:00:00 cmdDisp.CMD_WITH_MULTIPLE 1,, 2
R00:00:00 cmdDisp.CMD_WITH_MULTIPLE 1, 2,
R00:00:00 cmdDisp.CMD_WITH_MULTIPLE 1
R00:00:00 cmdDisp.CMD_ENUM ACTIVE
R00:00:00 cmdDisp.CMD_ENUM "STOPPED"
R00:00:00 cmdDisp.CMD_ENUM active
R00:00:00 cmdDisp.CMD_ENUM 1
R00:00:00 cmdDisp.CMD_ENUM [IDLE]
R00:00:00 cmdDisp.CMD_BOOL true
R00:00:00 cmdDisp.CMD_BOOL FALSE
R00:00:00 cmdDisp.CMD_BOOL yes
R00:00:00 cmdDisp.CMD_BOOL "No "
R00:00:00 cmdDisp.CMD_BOOL 1
R00:00:00 cmdDisp.CMD_BOOL tRue
R00:00:00 cmdDisp.CMD_ARRAY_INT [1, 2, 3]
R00:00:00 cmdDisp.CMD_ARRAY_INT [1, 2]
R00:00:00 cmdDisp.CMD_ARRAY_INT [1, 2, 3.0]
R00:00:00 cmdDisp.CMD_ARRAY_INT [1, true, "3"]
R00:00:00 cmdDisp.CMD_ARRAY_INT [1, 2, 2147483648]
R00:00:00 cmdDisp.CMD_ARRAY_INT "[1, 2, 3]"
R00:00:00 cmdDisp.CMD_ARRAY_INT "[1, 2,"
R00:00:00 cmdDisp.CMD_ARRAY_INT "7"
R00:00:00 cmdDisp.CMD_ARRAY_INT 7
R00:00:00 cmdDisp.CMD_ARRAY_INT name
R00:00:00 cmdDisp.CMD_ARRAY_INT []
R00:00:00 cmdDisp.CMD_ARRAY_INT [1, 2, 3
R00:00:00 cmdDisp.CMD_ARRAY_INT [0x10, -1, +2]
R00:00:00 cmdDisp.CMD_ARRAY_INT {a: 1}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1, y: 2}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {y: 2, x: 1}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1, z: 2}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1, y: 2, z: 3}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1, y: "2"}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1, y: 2.5}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1, x: 2, y: 3}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {"x": 1, "y": 2}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE '{"x": 1, "y": 2}'
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {x: 1 y: 2}
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE [1, 2]
R00:00:00 cmdDisp.CMD_OBJECT_SIMPLE {}
R00:00:00 cmdDisp.CMD_OBJECT_MIXED {num: 1, str: "s", flag: true}
R00:00:00 cmdDisp.CMD_OBJECT_MIXED {num: true, str: name, flag: 1}
R00:00:00 cmdDisp.CMD_OBJECT_MIXED {num: 1, str: "é", flag: False}
R00:00:00 cmdDisp.CMD_OBJECT_MIXED {num: 1, str: [1], flag: true}
R00:00:00 cmdDisp.CMD_ARRAY_OF_STRUCTS [{x: 1, y: 2}, {x: 3, y: 4}]
R00:00:00 cmdDisp.CMD_ARRAY_OF_STRUCTS [{x: 1, y: 2}, {x: 3}]
R00:00:00 cmdDisp.CMD_ARRAY_OF_STRUCTS [{x: 1, y: 2}, 3]
R00:00:00 cmdDisp.CMD_STRUCT_WITH_ARRAY {id: 1, points: [1, 2, 3]}
R00:00:00 cmdDisp.CMD_STRUCT_WITH_ARRAY {id: 1, points: [1, 2]}
R00:00:00 cmdDisp.CMD_STRUCT_WITH_ARRAY {id: 1, points: "[1, 2, 3]"}
R00:00:00 cmdDisp.CMD_WITH_MULTIPLE 1, [1, 2, 3], "s"
R00:00:00 cmdDisp.CMD_WITH_MULTIPLE x, [1, 2], 3
R00:00:00 cmdDisp.CMD_WITH_MULTIPLE 1 [1, 2, 3] "s"
; only a comment
\tR00:00:00 cmdDisp.CMD_NO_OP
R00:00:00 cmdDisp.CMD_NO_OP \\
R00:00:00 cmdDisp.CMD_NO_OP @
cmdDisp.CMD_NO_OP
R00:00:00
R00:00:00 cmdDisp.CMD_NO_OP R00:00:01 cmdDisp.CMD_NO_OP
""".strip("\n").split("\n")

CHARS = list(" \t\n,;:.[]{}\"'\\-+_$@#0123456789xXeEaARTtrue") + ["é", "\r", "\r\n", "R00:00:00 ", "true", "0x", "1e"]


def corpus(fixtures, count, rng):
    inputs = sorted((fixtures / "sequences/input").glob("*.seq"))
    texts = [p.read_text() for p in inputs if p.name != "invalid_utf8.seq"]
    cases = list(texts)
    cases += [line + "\n" for line in LINES]
    cases += [line for line in LINES]
    cases += ["", "\n", "\n\n; c\n", "R00:00:00 cmdDisp.CMD_NO_OP\r\nR00:00:01 cmdDisp.CMD_NO_OP\r", "\ufeffR00:00:00 cmdDisp.CMD_NO_OP\n"]
    lines = [l for t in texts for l in t.split("\n")] + LINES
    while len(cases) < count:
        kind = rng.randrange(4)
        if kind == 0:  # a file of random lines
            cases.append("\n".join(rng.choice(lines) for _ in range(rng.randrange(1, 6))) + rng.choice(["", "\n"]))
        elif kind == 1:  # a line with characters changed
            line = list(rng.choice(lines))
            for _ in range(rng.randrange(1, 4)):
                at = rng.randrange(len(line) + 1)
                action = rng.randrange(3)
                if action == 0 and line:
                    del line[min(at, len(line) - 1)]
                elif action == 1:
                    line.insert(at, rng.choice(CHARS))
                elif line:
                    line[min(at, len(line) - 1)] = rng.choice(CHARS)
            cases.append("".join(line) + "\n")
        elif kind == 2:  # a file cut short
            text = rng.choice(texts)
            cases.append(text[: rng.randrange(len(text) + 1)])
        else:  # tokens of two lines mixed
            a, b = rng.choice(lines).split(" "), rng.choice(lines).split(" ")
            cut = rng.randrange(len(a) + 1)
            cases.append(" ".join(a[:cut] + b[rng.randrange(len(b) + 1):]) + "\n")
    return cases


def python_result(source, dictionary, cont):
    output = source.with_suffix(".py.bin")
    output.unlink(missing_ok=True)
    printed = io.StringIO()
    try:
        with contextlib.redirect_stdout(printed):
            seqgen.generateSequence(str(source), str(output), str(dictionary), 0xFFFF, cont=cont)
    except seqgen.SeqGenException as exc:
        return "error", exc.getMsg()
    except Exception as exc:  # the Python tool crashes: any error will do
        return "crash", f"{type(exc).__name__}: {exc}"
    return "binary", (output.read_bytes(), printed.getvalue())


def fgds_result(fgds, source, dictionary, cont):
    output = source.with_suffix(".rs.bin")
    output.unlink(missing_ok=True)
    command = [fgds, "seqgen", str(source), str(output), "--dictionary", str(dictionary)] + (["--all-errors"] if cont else [])
    done = subprocess.run(command, capture_output=True, text=True, timeout=60)
    if done.returncode == 0:
        return "binary", (output.read_bytes(), done.stdout)
    if done.returncode != 1 or done.stderr or output.exists():
        return "crash", done.stderr
    return "error", done.stdout.rstrip("\n")


NESTED = re.compile(r":\d+: (Encountered syntax error parsing timestamp: )?Error trying to process rule")


def agree(kind, expected, got_kind, got, cont):
    """Whether the result of fgds is the result of the Python compiler; "more" when fgds reports more errors."""
    if kind == "crash":
        return got_kind == "error"
    if kind != got_kind:
        return False
    if kind == "binary" or got == expected:
        return True
    expected, got = NESTED.sub(":N: Error trying to process rule", expected), NESTED.sub(":N: Error trying to process rule", got)
    if got == expected:
        return True
    lines = iter(got.removeprefix("Parsing error: ").split("\n"))
    return cont and all(line in lines for line in expected.removeprefix("Parsing error: ").split("\n")) and "more"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fgds", required=True)
    parser.add_argument("--fixtures", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--count", type=int, default=3000)
    parser.add_argument("--seed", type=int, default=1)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    dictionary = (args.fixtures / "sequences/resources/enhanced_dictionary.json").resolve()
    rng = random.Random(args.seed)
    counts = {"binary": 0, "error": 0, "crash": 0, "more": 0}
    failures = []
    for index, text in enumerate(corpus(args.fixtures, args.count, rng)):
        source = (args.out / f"case{index % 8}.seq").resolve()
        source.write_text(text, newline="")
        cont = index % 3 == 2
        kind, expected = python_result(source, dictionary, cont)
        got_kind, got = fgds_result(args.fgds, source, dictionary, cont)
        same = agree(kind, expected, got_kind, got, cont)
        counts["more" if same == "more" else kind] += 1
        if not same:
            failures.append(index)
            kept = args.out / f"failure{index}.seq"
            kept.write_text(text, newline="")
            print(f"DIFFERENT {kept} (all errors: {cont})\n  python {kind}: {expected!r}\n  fgds   {got_kind}: {got!r}")
    total = sum(counts.values())
    print(f"  {total} sequences: {counts['binary']} compiled to the same bytes, {counts['error']} rejected with the same message, "
          f"{counts['more']} rejected with the same messages and more, {counts['crash']} that crash the Python compiler rejected; "
          f"{len(failures)} different")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
