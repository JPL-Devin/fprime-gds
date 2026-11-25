#!/usr/bin/env python3
"""Compare the offline tools of fgds with the Python tools on the dictionary of a deployment.

A sequence with every command of the dictionary, and the parameter database with every
default of the dictionary, are built by fprime-seqgen / fprime-prm-write and by fgds: the
files must be the same bytes. The database is decoded by fprime-prm-decode and by fgds in
each format, and damaged copies are given to both: both must accept or both must reject.

    python ref.py --fgds FGDS --dictionary DICTIONARY --out DIR      (Python tools on PATH)
"""
import argparse
import json
import math
import random
import subprocess
import sys
import zlib
from pathlib import Path

failures = []


def decoded(text):
    """The values of a decoded JSON file; the spelling of a number or of a character is not compared."""
    def plain(value):
        if isinstance(value, float) and (math.isnan(value) or math.isinf(value)):
            return {"nan": "NaN", "inf": "Infinity", "-inf": "-Infinity"}[str(value)]
        if isinstance(value, dict):
            return {key: plain(item) for key, item in value.items()}
        return [plain(item) for item in value] if isinstance(value, list) else value
    return plain(json.loads(text))


def run(*command):
    done = subprocess.run([str(c) for c in command], capture_output=True, text=True, timeout=120)
    if "panicked" in done.stderr:
        failures.append(f"panic: {command}\n{done.stderr}")
    return done


def same(what, a, b, read=Path.read_bytes):
    if not (a.exists() and b.exists() and read(a) == read(b)):
        failures.append(f"{what}: {a} and {b} differ")
        return False
    return True


# A decoded JSON file is compared by its values, the other forms by their bytes.
VALUES = {"json": {"read": lambda path: decoded(path.read_text())}}


def sample(type_ref, types, rng):
    """A value of a type, as written in a sequence file; None when no value is offered."""
    kind, name = type_ref["kind"], type_ref["name"]
    if kind == "integer":
        bits, signed = type_ref["size"], type_ref["signed"]
        low, high = (-(1 << (bits - 1)), (1 << (bits - 1)) - 1) if signed else (0, (1 << bits) - 1)
        return str(rng.choice([low, high, 0, 1, rng.randint(low, high)]))
    if kind == "float":
        return rng.choice(["0.0", "1.5", "-2.25", "1e3", "7"])
    if kind == "bool":
        return rng.choice(["true", "false"])
    if kind == "string":
        return json.dumps("".join(rng.choice("abc xyz_/.é") for _ in range(rng.randint(0, min(type_ref.get("size", 8), 12)))))
    definition = types.get(name)
    if definition is None:
        return None
    if definition["kind"] == "alias":
        return sample(definition["underlyingType"], types, rng)
    if definition["kind"] == "enum":
        return rng.choice(definition["enumeratedConstants"])["name"]
    if definition["kind"] == "array":
        items = [sample(definition["elementType"], types, rng) for _ in range(definition["size"])]
        return None if None in items else "[" + ", ".join(items) + "]"
    if definition["kind"] == "struct":
        members = []
        for member, spec in definition["members"].items():
            if "size" in spec:
                items = [sample(spec["type"], types, rng) for _ in range(spec["size"])]
                value = None if None in items else "[" + ", ".join(items) + "]"
            else:
                value = sample(spec["type"], types, rng)
            if value is None:
                return None
            members.append(f"{member}: {value}")
        return "{" + ", ".join(members) + "}"
    return None


def sequences(args, dictionary):
    types = {t["qualifiedName"]: t for t in dictionary["typeDefinitions"]}
    rng = random.Random(7)
    lines, skipped = ["; every command of the dictionary"], 0
    for index, command in enumerate(dictionary["commands"]):
        for _ in range(3):
            values = [sample(p["type"], types, rng) for p in command["formalParams"]]
            if None in values:
                skipped += 1
                break
            tag = f"R00:00:{index % 60:02d}.{index % 4 * 25:02d}" if index % 2 else f"A2024-{1 + index % 366:03d}T01:02:{index % 60:02d}"
            lines.append(f"{tag} {command['name']} {', '.join(values)}".rstrip())
    # Each line alone, so that a line one tool rejects does not hide the others.
    accepted, rejected = [], 0
    for number, line in enumerate(lines[1:]):
        source = args.out / "line.seq"
        source.write_text(line + "\n")
        python = run("fprime-seqgen", source, args.out / "line.py.bin", "--dictionary", args.dictionary)
        rust = run(args.fgds, "seqgen", source, args.out / "line.rs.bin", "--dictionary", args.dictionary)
        if (python.returncode == 0) != (rust.returncode == 0):
            failures.append(f"command line {line!r}: fprime-seqgen exit {python.returncode}, fgds exit {rust.returncode}\n{python.stdout}{rust.stderr}")
        elif python.returncode == 0:
            if same(f"command line {line!r}", args.out / "line.py.bin", args.out / "line.rs.bin"):
                accepted.append(line)
        else:
            rejected += 1
    source = args.out / "commands.seq"
    source.write_text("\n".join(accepted) + "\n")
    python = run("fprime-seqgen", source, args.out / "commands.py.bin", "--dictionary", args.dictionary, "-t", "2")
    rust = run(args.fgds, "seqgen", source, args.out / "commands.rs.bin", "--dictionary", args.dictionary, "-t", "2")
    if python.returncode or rust.returncode:
        failures.append(f"commands.seq: fprime-seqgen exit {python.returncode}, fgds exit {rust.returncode}\n{python.stdout}{rust.stderr}")
    else:
        same("commands.seq", args.out / "commands.py.bin", args.out / "commands.rs.bin")
    print(f"  {len(dictionary['commands'])} commands: {len(accepted)} lines compiled to the same bytes, alone and as one "
          f"sequence of {(args.out / 'commands.py.bin').stat().st_size} bytes; {rejected} rejected by both; "
          f"{skipped} commands without a sample value")


# Values of the enum, array and struct parameters of Ref, which have no default.
TYPED = {
    "Ref.typeDemo": {
        "CHOICE_PRM": "TWO",
        "CHOICES_PRM": ["ONE", "BLUE"],
        "EXTRA_CHOICES_PRM": ["RED", "RED"],
        "CHOICE_PAIR_PRM": {"firstChoice": "ONE", "secondChoice": "RED"},
        "GLUTTON_OF_CHOICE_PRM": {
            "tooManyChoices": [["ONE", "TWO"], ["RED", "BLUE"]],
            "separateChoice": "BLUE",
            "choicePair": {"firstChoice": "TWO", "secondChoice": "ONE"},
            "choiceAsMemberArray": [7, 255],
        },
    }
}


def said(tool, output, done):
    """What a tool prints is the same for both tools, the name of its output file apart."""
    key = str(output).replace(f".{tool}.", ".")
    text = (done.stdout + done.stderr).replace(str(output), key)
    if done.returncode == 0 and printed.setdefault(key, text) != text:
        failures.append(f"printed for {key}:\n{printed[key]}and\n{text}")


printed = {}


def typed(args, dictionary):
    """Parameters of enum, array and struct type: the database, the sequence and the decoded JSON."""
    names = {p["name"] for p in dictionary["parameters"]}
    wanted = {f"{c}.{p}" for c, values in TYPED.items() for p in values}
    if not wanted <= names:
        print("  the dictionary has no Ref.typeDemo parameters: enum, array and struct parameters not compared")
        return
    source = args.out / "typed.json"
    source.write_text(json.dumps(TYPED))
    for form in ("dat", "seq"):
        outputs = []
        for tool, command in (("py", ["fprime-prm-write"]), ("rs", [args.fgds, "prm-write"])):
            output = args.out / f"typed.{tool}.{form}"
            done = run(*command, form, source, "--dictionary", args.dictionary, "-o", output)
            said(tool, output, done)
            if done.returncode:
                failures.append(f"typed prm-write {form} ({tool}): exit {done.returncode}\n{done.stdout}{done.stderr}")
            outputs.append(output)
        same(f"typed prm-write {form}", *outputs)
    # The sequence form of a struct value is not sequence syntax (quoted member names): both
    # compilers must say the same of it.
    compiled = []
    for tool, command in (("py", ["fprime-seqgen"]), ("rs", [args.fgds, "seqgen"])):
        output = args.out / f"typed.seq.{tool}.bin"
        output.unlink(missing_ok=True)
        done = run(*command, args.out / "typed.py.seq", output, "--dictionary", args.dictionary)
        said(tool, output, done)
        message = (done.stdout + done.stderr).strip().split("\n")[-1]
        compiled.append((done.returncode != 0, message if done.returncode else output.read_bytes()))
    if compiled[0] != compiled[1]:
        failures.append(f"the typed parameter sequence, compiled: {compiled[0]} and {compiled[1]}")
    # Decoded: the files of both tools are the same bytes (an array or struct is shown by the Python
    # tool as the description of its type in text and CSV; fgds does the same).
    for form in ("json", "text", "csv"):
        outputs = []
        for tool, command in (("py", ["fprime-prm-decode"]), ("rs", [args.fgds, "prm-decode"])):
            output = args.out / f"typed.decoded.{tool}.{form}"
            done = run(*command, args.out / "typed.py.dat", "--dictionary", args.dictionary, "-f", form, "-o", output)
            said(tool, output, done)
            if done.returncode:
                failures.append(f"typed prm-decode {form} ({tool}): exit {done.returncode}\n{done.stdout}{done.stderr}")
            outputs.append(output)
        # The CSV of fprime-prm-decode does not quote a field that holds quotes and commas: not compared.
        if form != "csv":
            same(f"typed prm-decode {form}", *outputs, **VALUES.get(form, {}))
    print(f"  {len(wanted)} enum, array and struct parameters: the same .dat ({(args.out / 'typed.py.dat').stat().st_size} bytes) and "
          f"sequence; the sequence {'rejected by both compilers with the same message' if compiled[0][0] else 'compiled to the same bytes'}; "
          f"decoded to the same values (JSON) and text")


def parameters(args, dictionary):
    source = args.out / "params.json"
    source.write_text("{}")
    for form, options in (("dat", []), ("seq", []), ("seq", ["--save"])):
        outputs = []
        for tool, command in (("py", ["fprime-prm-write"]), ("rs", [args.fgds, "prm-write"])):
            output = args.out / f"params{''.join(options)}.{tool}.{form}"
            done = run(*command, form, source, "--dictionary", args.dictionary, "--defaults", *options, "-o", output)
            said(tool, output, done)
            if done.returncode:
                failures.append(f"prm-write {form} ({tool}): exit {done.returncode}\n{done.stdout}{done.stderr}")
            outputs.append(output)
        same(f"prm-write {form} {options}", *outputs)
    typed(args, dictionary)
    dat = args.out / "params.py.dat"
    if not dat.exists():
        return
    # The sequence the parameter tool writes compiles to the same bytes with both compilers.
    for tool, command in (("py", ["fprime-seqgen"]), ("rs", [args.fgds, "seqgen"])):
        run(*command, args.out / "params.py.seq", args.out / f"params.seq.{tool}.bin", "--dictionary", args.dictionary)
    same("the parameter sequence, compiled", args.out / "params.seq.py.bin", args.out / "params.seq.rs.bin")
    count = 0
    for form in ("json", "text", "csv"):
        outputs = []
        for tool, command in (("py", ["fprime-prm-decode"]), ("rs", [args.fgds, "prm-decode"])):
            output = args.out / f"decoded.{tool}.{form}"
            done = run(*command, dat, "--dictionary", args.dictionary, "-f", form, "-o", output)
            said(tool, output, done)
            if done.returncode:
                failures.append(f"prm-decode {form} ({tool}): exit {done.returncode}\n{done.stdout}{done.stderr}")
            outputs.append(output)
        if form == "json":
            # Round trip: the decoded file written again is the same database.
            count = sum(len(v) for v in json.loads(outputs[0].read_text()).values())
            again = args.out / "again.dat"
            run(args.fgds, "prm-write", "dat", outputs[1], "--dictionary", args.dictionary, "-o", again)
            same("decode then write", dat, again)
        same(f"prm-decode {form}", *outputs, **VALUES.get(form, {}))
    # Damaged databases: cut short, or one byte changed, with and without a correct checksum.
    data = dat.read_bytes()
    rng = random.Random(11)
    damaged = [data[:n] for n in range(0, min(len(data), 40))] + [data[: rng.randrange(len(data))] for _ in range(20)]
    for _ in range(60):
        body = bytearray(data[4:])
        body[rng.randrange(len(body))] ^= rng.choice([1, 0x80, 0xFF])
        damaged.append(data[:4] + bytes(body))
        damaged.append(((zlib.crc32(bytes(body)) ^ 0xFFFFFFFF) & 0xFFFFFFFF).to_bytes(4, "big") + bytes(body))
    for body in (data[4:n] for n in range(4, len(data), max(1, len(data) // 40))):
        damaged.append(((zlib.crc32(body) ^ 0xFFFFFFFF) & 0xFFFFFFFF).to_bytes(4, "big") + body)
    agree = {"accepted": 0, "rejected": 0, "string": 0}
    for index, content in enumerate(damaged):
        bad = args.out / "damaged.dat"
        bad.write_bytes(content)
        results = []
        for tool, command in (("py", ["fprime-prm-decode"]), ("rs", [args.fgds, "prm-decode"])):
            output = args.out / f"damaged.{tool}.json"
            output.unlink(missing_ok=True)
            done = run(*command, bad, "--dictionary", args.dictionary, "-o", output)
            said(tool, output, done)
            results.append((done.returncode == 0, decoded(output.read_text()) if done.returncode == 0 else None))
            message = done.stderr
        # fprime-prm-decode reads the length of a string as U16 whatever FwSizeStoreType is in the
        # dictionary (docs/design/deviations.md), so it does not see damage to the rest of the length.
        if results[0][0] and not results[1][0] and "deserialize string" in message:
            agree["string"] += 1
        elif results[0] != results[1]:
            kept = args.out / f"damaged{index}.dat"
            kept.write_bytes(content)
            failures.append(f"{kept}: fprime-prm-decode {'accepts' if results[0][0] else 'rejects'}, fgds {'accepts' if results[1][0] else 'rejects'} or the output differs")
        else:
            agree["accepted" if results[0][0] else "rejected"] += 1
    print(f"  {count} parameters with a default: the same .dat ({len(data)} bytes), sequence, and decoded JSON, text and CSV; "
          f"{len(damaged)} damaged databases: {agree['rejected']} rejected by both, {agree['accepted']} decoded the same, "
          f"{agree['string']} with a damaged string length rejected by fgds only")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--fgds", required=True)
    parser.add_argument("--dictionary", required=True)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    dictionary = json.loads(Path(args.dictionary).read_text())
    sequences(args, dictionary)
    parameters(args, dictionary)
    for failure in failures:
        print("DIFFERENT", failure)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
