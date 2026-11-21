#!/usr/bin/env python3
"""Generate the expected outputs of `fprime-dp` for every data product fixture.

Usage: gen_dp_expected.py <fixtures dir>

Runs the `fprime-dp` executable installed next to the running Python interpreter. Every command runs
in a scratch directory holding a copy of `dp/test_dp_data`, with relative paths, so the captured
text does not depend on the machine. The scratch directory path is replaced by `<WORKDIR>` wherever
the tool prints an absolute path.
"""
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

FIXTURES = Path(sys.argv[1]).resolve()
DATA = FIXTURES / "dp" / "test_dp_data"
EXPECTED = FIXTURES / "dp" / "expected"
FPRIME_DP = str(Path(sys.executable).parent / "fprime-dp")
DICTIONARY_FOR_SUFFIX = {".bin": "dictionary.json", ".fdp": "dictionary_ref.json"}
DATA_DIR_NAME = "test_dp_data"


def run(args, cwd):
    """Run fprime-dp, return (exit code, stdout, stderr) with the scratch path masked"""
    result = subprocess.run([FPRIME_DP, *args], cwd=cwd, capture_output=True, text=True)
    mask = lambda text: text.replace(str(Path(cwd).resolve()), "<WORKDIR>")
    return result.returncode, mask(result.stdout), mask(result.stderr)


def write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n")


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def product_modes(product, dictionary, header_size):
    """(mode name, arguments, output file or None) for one product file"""
    binary = f"{DATA_DIR_NAME}/{product}"
    dict_path = f"{DATA_DIR_NAME}/{dictionary}"
    return [
        ("decode", ["decode", "-b", binary, "-d", dict_path, "-o", "out.json"], "out.json"),
        ("decode_no_decompression", ["decode", "-z", "-b", binary, "-d", dict_path, "-o", "out.json"], "out.json"),
        ("validate_dictionary", ["validate", "-b", binary, "-d", dict_path], None),
        ("validate_dictionary_verbose", ["validate", "-v", "-b", binary, "-d", dict_path], None),
        ("validate_header_size", ["validate", "-b", binary, "-s", str(header_size)], None),
        ("validate_header_size_verbose", ["validate", "-v", "-b", binary, "-s", str(header_size)], None),
        ("validate_wrong_header_size", ["validate", "-b", binary, "-s", str(header_size + 3)], None),
        ("validate_guess", ["validate", "-g", "-b", binary], None),
        ("validate_guess_verbose", ["validate", "-g", "-v", "-b", binary], None),
        ("validate_no_option", ["validate", "-b", binary], None),
    ]


def generate_products():
    products = sorted(path.name for path in DATA.iterdir() if path.suffix in DICTIONARY_FOR_SUFFIX)
    for product in products:
        dictionary = DICTIONARY_FOR_SUFFIX[Path(product).suffix]
        out_dir = EXPECTED / product
        results = []
        with tempfile.TemporaryDirectory() as work:
            shutil.copytree(DATA, Path(work) / DATA_DIR_NAME)
            _, verbose_out, _ = run(["validate", "-v", "-b", f"{DATA_DIR_NAME}/{product}", "-d", f"{DATA_DIR_NAME}/{dictionary}"], work)
            header_size = int(re.search(r"Calculated a header size of (\d+)", verbose_out).group(1))
            for mode, args, output in product_modes(product, dictionary, header_size):
                produced = Path(work) / output if output else None
                if produced is not None and produced.exists():
                    produced.unlink()
                exit_code, stdout, stderr = run(args, work)
                entry = {"mode": mode, "args": args, "exit_code": exit_code, "stdout": stdout, "stderr": stderr}
                if output is not None:
                    if produced.exists():
                        shutil.copy(produced, out_dir.mkdir(parents=True, exist_ok=True) or out_dir / f"{mode}.json")
                        entry["output_json"] = f"{mode}.json"
                    else:
                        entry["output_json"] = None
                results.append(entry)
        write_json(
            out_dir / "expected.json",
            {"product": product, "dictionary": dictionary, "header_size": header_size, "sha256": sha256(DATA / product), "runs": results},
        )
    return products


# Command-line cases of test/fprime_gds/executables/test_data_products.py. `stage` lists the files
# put in the scratch directory `in/` before the run: [source in test_dp_data, staged name, offset of
# one byte to invert or None].
CONTAINER = "00002576"
HEADER_OFFSET, RECORD_OFFSET = 2, -10
UNCOMPRESSED, COMPRESSED = "DpDemoUncompressed.fdp", "DpDemoCompressed.fdp"
DICT_REF = ["-d", f"{DATA_DIR_NAME}/dictionary_ref.json"]
CLI_CASES = [
    ("file_default_output_name", [[UNCOMPRESSED, f"Dp_{CONTAINER}_00000010_00000020.fdp", None]], ["-b", f"in/Dp_{CONTAINER}_00000010_00000020.fdp"]),
    ("file_explicit_output", [[UNCOMPRESSED, "product.fdp", None]], ["-b", "in/product.fdp", "-o", "in/custom.json"]),
    ("file_rename_id", [[UNCOMPRESSED, f"Dp_{CONTAINER}_00000010_00000020.fdp", None]], ["-b", f"in/Dp_{CONTAINER}_00000010_00000020.fdp", "--rename-id"]),
    ("file_rename_id_explicit_output_wins", [[UNCOMPRESSED, f"Dp_{CONTAINER}_00000010_00000020.fdp", None]], ["-b", f"in/Dp_{CONTAINER}_00000010_00000020.fdp", "--rename-id", "-o", "in/custom.json"]),
    ("file_rename_id_downlinked_name", [[UNCOMPRESSED, f"._DpCat_Dp_{CONTAINER}_00000010_00000020.fdp", None]], ["-b", f"in/._DpCat_Dp_{CONTAINER}_00000010_00000020.fdp", "--rename-id"]),
    ("file_rename_id_unknown_container", [[UNCOMPRESSED, "Dp_00000001_00000010_00000020.fdp", None]], ["-b", "in/Dp_00000001_00000010_00000020.fdp", "--rename-id"]),
    ("file_rename_id_unconventional_name", [[UNCOMPRESSED, UNCOMPRESSED, None]], ["-b", f"in/{UNCOMPRESSED}", "--rename-id"]),
    ("file_header_crc_failure", [[UNCOMPRESSED, "bad.fdp", HEADER_OFFSET]], ["-b", "in/bad.fdp"]),
    ("file_record_deserialize_failure", [[UNCOMPRESSED, "bad.fdp", RECORD_OFFSET]], ["-b", "in/bad.fdp"]),
    ("file_missing", [], ["-b", "in/missing.fdp"]),
    ("dir_decodes_every_fdp", [[UNCOMPRESSED, f"Dp_{CONTAINER}_00000010_00000020.fdp", None], [COMPRESSED, f"Dp_{CONTAINER}_00000011_00000021.fdp", None], [UNCOMPRESSED, "not-a-product.bin", None]], ["--bin-dir", "in"]),
    ("dir_rename_id", [[UNCOMPRESSED, f"Dp_{CONTAINER}_00000010_00000020.fdp", None], [COMPRESSED, f"Dp_{CONTAINER}_00000011_00000021.fdp", None]], ["--bin-dir", "in", "-r"]),
    ("dir_no_decompression", [[UNCOMPRESSED, f"Dp_{CONTAINER}_00000010_00000020.fdp", None], [COMPRESSED, f"Dp_{CONTAINER}_00000011_00000021.fdp", None]], ["--bin-dir", "in", "-z"]),
    ("dir_continues_past_failures", [[UNCOMPRESSED, "Dp_00002576_00000009_00000019.fdp", HEADER_OFFSET], [UNCOMPRESSED, "Dp_00002576_00000010_00000020.fdp", None], [UNCOMPRESSED, "Dp_00002576_00000011_00000021.fdp", RECORD_OFFSET]], ["--bin-dir", "in"]),
    ("dir_rename_id_output_collision", [[UNCOMPRESSED, "Dp_00002576_00000010_00000020.fdp", None], [COMPRESSED, "Dp_2576_00000010_00000020.fdp", None]], ["--bin-dir", "in", "-r"]),
    ("dir_rename_id_collision_after_failure", [[UNCOMPRESSED, "Dp_00002576_00000010_00000020.fdp", HEADER_OFFSET], [UNCOMPRESSED, "Dp_2576_00000010_00000020.fdp", None]], ["--bin-dir", "in", "-r"]),
    ("dir_single_file_summary", [[UNCOMPRESSED, "Dp_00002576_00000010_00000020.fdp", None]], ["--bin-dir", "in"]),
    ("dir_empty", [], ["--bin-dir", "in"]),
    ("dir_missing", [], ["--bin-dir", "in/missing"]),
    ("dir_output_rejected", [], ["--bin-dir", "in", "-o", "in/x.json"]),
    ("dir_and_file_exclusive", [], ["--bin-dir", "in", "-b", "in/x.fdp"]),
    ("input_required", [], []),
]


def generate_cli_cases(products):
    known = {}
    for product in products:
        for name in ("decode.json", "decode_no_decompression.json"):
            path = EXPECTED / product / name
            if path.exists():
                known.setdefault(sha256(path), f"{product}/{name}")
    cases = []
    extra_dir = EXPECTED / "cli_cases"
    for name, stage, extra_args in CLI_CASES:
        with tempfile.TemporaryDirectory() as work:
            shutil.copytree(DATA, Path(work) / DATA_DIR_NAME)
            staging = Path(work) / "in"
            staging.mkdir()
            for source, staged_name, corrupt_offset in stage:
                raw = bytearray((DATA / source).read_bytes())
                if corrupt_offset is not None:
                    raw[corrupt_offset] ^= 0xFF
                (staging / staged_name).write_bytes(raw)
            args = ["decode", *DICT_REF, *extra_args]
            exit_code, stdout, stderr = run(args, work)
            outputs = []
            for path in sorted(staging.glob("*.json")):
                digest = sha256(path)
                entry = {"name": path.name, "sha256": digest}
                if digest in known:
                    entry["identical_to"] = known[digest]
                else:
                    target = extra_dir / name / path.name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(path, target)
                    entry["identical_to"] = f"cli_cases/{name}/{path.name}"
                outputs.append(entry)
            cases.append(
                {
                    "name": name,
                    "stage": [{"source": s, "name": n, "invert_byte_at": o} for s, n, o in stage],
                    "args": args,
                    "exit_code": exit_code,
                    "stdout": stdout,
                    "stderr": stderr,
                    "json_files_written": outputs,
                }
            )
    write_json(EXPECTED / "cli_cases.json", {"cases": cases})


if __name__ == "__main__":
    if EXPECTED.exists():
        shutil.rmtree(EXPECTED)
    generate_cli_cases(generate_products())
