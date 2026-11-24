#!/usr/bin/env python3
"""`fgds dp` and the data products routes against the ground truth of fixtures/dp.

Usage: check.py <fgds binary with the plugin> <fixtures/dp> <scratch directory> [--fprime-dp <fprime-dp>]

The comparison rules are in tests/dp/README.md.
"""
import hashlib
import json
import os
import random
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

FGDS, FIXTURES, OUT = sys.argv[1:4]
FPRIME_DP = sys.argv[5] if len(sys.argv) > 5 and sys.argv[4] == "--fprime-dp" else None
DATA = os.path.join(FIXTURES, "test_dp_data")
EXPECTED = os.path.join(FIXTURES, "expected")


def step(text):
    print(f"  ok: {text}", flush=True)


def scratch(name):
    """A directory holding a copy of test_dp_data, as the fixtures were generated in."""
    path = os.path.join(OUT, name)
    shutil.rmtree(path, ignore_errors=True)
    os.makedirs(path)
    shutil.copytree(DATA, os.path.join(path, "test_dp_data"))
    return path


def run(program, args, cwd):
    result = subprocess.run([*program, *args], cwd=cwd, capture_output=True, text=True, timeout=120)
    mask = lambda text: text.replace(os.path.realpath(cwd), "<WORKDIR>")
    return result.returncode, mask(result.stdout), mask(result.stderr)


def same_json(a, b):
    """Equal parsed values; NaN equals NaN."""
    load = lambda text: json.loads(text, parse_constant=lambda name: name)
    return load(a) == load(b)


def read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def sha256(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


def check_products():
    """Every product in every recorded mode: exit code, output text, JSON written."""
    runs = files = 0
    for product in sorted(os.listdir(EXPECTED)):
        if not os.path.isdir(os.path.join(EXPECTED, product)):
            continue
        expected = json.loads(read(os.path.join(EXPECTED, product, "expected.json")))
        for mode in expected["runs"]:
            cwd = scratch("product")
            code, stdout, stderr = run([FGDS, "dp"], mode["args"], cwd)
            what = f"{product} {mode['mode']}"
            assert (code, stdout, stderr) == (mode["exit_code"], mode["stdout"], mode["stderr"]), (what, code, stdout, stderr)
            written = os.path.join(cwd, "out.json")
            if mode.get("output_json"):
                theirs = read(os.path.join(EXPECTED, product, mode["output_json"]))
                assert same_json(read(written), theirs), f"{what}: decoded values differ"
                assert read(written) == theirs, f"{what}: the JSON file differs in bytes"
                files += 1
            else:
                assert not os.path.exists(written), f"{what}: a file was written"
            runs += 1
    assert (runs, files) == (180, 32), (runs, files)
    step(f"{runs} runs over 18 products x 10 modes equal fprime-dp (exit code, stdout, stderr); {files} JSON files identical")


def check_cli_cases():
    cases = json.loads(read(os.path.join(EXPECTED, "cli_cases.json")))["cases"]
    for case in cases:
        cwd = scratch("case")
        os.makedirs(os.path.join(cwd, "in"))
        for staged in case["stage"]:
            with open(os.path.join(DATA, staged["source"]), "rb") as handle:
                content = bytearray(handle.read())
            if staged["invert_byte_at"] is not None:
                content[staged["invert_byte_at"]] ^= 0xFF
            with open(os.path.join(cwd, "in", staged["name"]), "wb") as handle:
                handle.write(content)
        code, stdout, stderr = run([FGDS, "dp"], case["args"], cwd)
        name = case["name"]
        if case["exit_code"] == 2:
            # A command line that is refused: the wording and the exit code are fgds's own.
            assert code != 0 and stdout == "" and stderr, (name, code, stdout, stderr)
        else:
            assert (code, stdout) == (case["exit_code"], case["stdout"]), (name, code, stdout, stderr)
            assert stderr == case["stderr"], (name, stderr)
        written = sorted(f for f in os.listdir(os.path.join(cwd, "in")) if f.endswith(".json"))
        assert written == sorted(f["name"] for f in case["json_files_written"]), (name, written)
        for file in case["json_files_written"]:
            assert sha256(os.path.join(cwd, "in", file["name"])) == file["sha256"], (name, file["name"])
    assert len(cases) == 22, len(cases)
    step(f"{len(cases)} command-line cases equal fprime-dp (exit code, stdout, stderr, files written by SHA-256)")


def error_class(stderr):
    """What kind of failure the tool reported: the text before the first colon of each line."""
    return [line.split(":", 1)[0] for line in stderr.splitlines() if not line.startswith((" ", "Traceback"))][:1]


def check_against_live_tool():
    """Damaged products through both tools: same exit code, same output, same kind of error."""
    generator = random.Random(20261121)
    products = sorted(f for f in os.listdir(DATA) if not f.endswith(".json"))
    cwd = scratch("live")
    compared = decoded = 0
    for product in products:
        dictionary = "test_dp_data/dictionary.json" if product.endswith(".bin") else "test_dp_data/dictionary_ref.json"
        with open(os.path.join(DATA, product), "rb") as handle:
            original = handle.read()
        for _ in range(12):
            damaged = bytearray(original)
            kind = generator.randrange(4)
            if kind == 0:
                damaged = damaged[: generator.randrange(len(damaged))]
            elif kind == 1:
                damaged[generator.randrange(len(damaged))] ^= 1 << generator.randrange(8)
            elif kind == 2:
                at = generator.randrange(len(damaged))
                damaged[at : at + generator.randrange(1, 9)] = bytes(generator.randrange(256) for _ in range(4))
            else:
                at = generator.randrange(60, len(damaged))
                damaged[at] = generator.randrange(256)
            with open(os.path.join(cwd, "damaged.fdp"), "wb") as handle:
                handle.write(damaged)
            for args in (
                ["decode", "-b", "damaged.fdp", "-d", dictionary, "-o", "out.json"],
                ["validate", "-b", "damaged.fdp", "-d", dictionary],
                ["validate", "-b", "damaged.fdp", "-g", "-v"],
            ):
                results = []
                for program in ([FGDS, "dp"], [FPRIME_DP]):
                    out = os.path.join(cwd, "out.json")
                    if os.path.exists(out):
                        os.remove(out)
                    code, stdout, stderr = run(program, args, cwd)
                    results.append((code, stdout, error_class(stderr), read(out) if os.path.exists(out) else None, stderr))
                ours, theirs = results
                what = (product, kind, args[0], ours, theirs)
                assert ours[:3] == theirs[:3], what
                assert (ours[3] is None) == (theirs[3] is None), what
                if ours[3] is not None:
                    assert same_json(ours[3], theirs[3]) and ours[3] == theirs[3], what
                    decoded += 1
                # Where the Python tool reports a fault of the product, the text is the same.
                if theirs[2] in (["Error"], ["Invalid Header checksum. Checksum in file"], ["No valid checksum found"]):
                    assert ours[4] == theirs[4], what
                compared += 1
    step(f"{compared} runs on damaged products equal a live fprime-dp ({decoded} still decoded, identical JSON)")


_GIVEN = set()


def free_port():
    """A free port; never the same one twice in this process."""
    while True:
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        if port not in _GIVEN:
            _GIVEN.add(port)
            return port


def check_server():
    """The watched directory through the API: listed once, decoded on demand, missing when gone."""
    cwd = scratch("server")
    watched = os.path.join(cwd, "watched")
    os.makedirs(os.path.join(watched, "copy"))
    http, link = free_port(), free_port()
    api = f"http://127.0.0.1:{http}"
    command = [FGDS, "run", "--logs", os.path.join(cwd, "logs"), "--dictionary", "test_dp_data/dictionary_ref.json", "--bind", f"127.0.0.1:{http}",
               "--comm-tcp-address", f"127.0.0.1:{link}", "--data-products-dir", watched, "--data-products-settle-ms", "300"]
    log = open(os.path.join(cwd, "fgds.log"), "w")
    server = subprocess.Popen(command, cwd=cwd, stdout=log, stderr=subprocess.STDOUT)

    def get(path, raw=False):
        try:
            with urllib.request.urlopen(api + path, timeout=10) as response:
                body = response.read()
                return (response.status, body, response.headers) if raw else json.loads(body)
        except urllib.error.HTTPError as error:
            body = error.read()
            return (error.code, body, error.headers) if raw else json.loads(body)

    def wait_for(what, condition):
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                value = condition()
                if value:
                    return value
            except (OSError, KeyError):
                pass
            time.sleep(0.1)
        raise AssertionError(f"timed out waiting for {what}; see {cwd}/fgds.log")

    def products():
        return {p["hash"]: p for p in get("/api/v1/data-products")["products"]}

    try:
        wait_for("fgds", lambda: get("/api/v1/version"))
        tabs = [tab for tab in get("/ui/tabs") if tab["id"] == "data-products"]
        assert tabs and tabs[0]["title"] == "Data Products", tabs
        status, module, _ = get(tabs[0]["module"], raw=True)
        assert status == 200 and b'customElements.define("data-products-tab"' in module
        status, lit, headers = get("/ui/plugins/data-products/lit-core.min.js", raw=True)
        assert status == 200 and headers["content-type"].startswith("text/javascript") and b"LitElement" in lit
        assert b"unsafeHTML" not in module and b"innerHTML" not in module
        step("the tab is listed and its module and Lit are served from the binary")

        shutil.copy(os.path.join(DATA, "DpDemoUncompressed.fdp"), os.path.join(watched, "Dp_00002576_00000010_00000020.fdp"))
        shutil.copy(os.path.join(DATA, "DpDemoUncompressed.fdp"), os.path.join(watched, "copy", "same.fdp"))
        shutil.copy(os.path.join(DATA, "DpDemoCompressed.fdp"), os.path.join(watched, "compressed.fdp"))
        with open(os.path.join(DATA, "DpDemoUncompressed.fdp"), "rb") as handle:
            damaged = bytearray(handle.read())
        damaged[-10] ^= 0xFF
        with open(os.path.join(watched, "damaged.fdp"), "wb") as handle:
            handle.write(damaged)
        with open(os.path.join(watched, "<img src=x onerror=alert(1)>.fdp"), "wb") as handle:
            handle.write(b"not a product")
        os.symlink(os.path.join(DATA, "makeU32.bin"), os.path.join(watched, "link.fdp"))
        listed = wait_for("4 products", lambda: len(products()) == 4 and products())
        whole = sha256(os.path.join(DATA, "DpDemoUncompressed.fdp"))
        packed = sha256(os.path.join(DATA, "DpDemoCompressed.fdp"))
        assert sorted(s["path"] for s in listed[whole]["sources"]) == ["Dp_00002576_00000010_00000020.fdp", "copy/same.fdp"]
        assert listed[whole]["container"] == "Ref.dpDemo.DpDemoContainer" and listed[whole]["valid"] is True
        assert listed[whole]["records"] == 14 and listed[packed]["records"] == 14 and listed[packed]["proc_types"] == 1
        bad = listed[hashlib.sha256(damaged).hexdigest()]
        assert bad["valid"] is False and bad["error"].startswith("Invalid Data checksum."), bad
        assert sum(1 for p in listed.values() if not p["valid"]) == 2
        records = get("/api/v1/records/latest?kind=fprime.data_product&filter=valid:eq:true")["records"]
        assert sorted(r["entity"] for r in records) == sorted([whole, packed]), records
        step("4 products indexed from 5 files: equal content listed once with both paths, 2 invalid with their error, the link ignored")

        for product, file, query in (
            (whole, "DpDemoUncompressed.fdp/decode.json", ""),
            (packed, "DpDemoCompressed.fdp/decode.json", ""),
            (packed, "DpDemoCompressed.fdp/decode_no_decompression.json", "?decompress=false"),
        ):
            status, body, headers = get(f"/api/v1/data-products/{product}/export{query}", raw=True)
            assert status == 200 and body.decode() == read(os.path.join(EXPECTED, file)), (file, status)
            assert headers["content-disposition"].startswith("attachment; filename="), headers
            detail = get(f"/api/v1/data-products/{product}{query}")
            assert detail["error"] is None and detail["decoded"] == json.loads(read(os.path.join(EXPECTED, file))), file
        status, body, _ = get(f"/api/v1/data-products/{bad['hash']}/export", raw=True)
        assert status == 422 and json.loads(body)["error"]["code"] == "decode_failed", body
        assert get(f"/api/v1/data-products/{bad['hash']}")["error"]["message"].startswith("Field 'nestedArrays'")
        assert get("/api/v1/data-products/0000", raw=True)[0] == 404
        step("export equals the fprime-dp JSON byte for byte (also without decompression); a product that does not decode gives its error")

        os.remove(os.path.join(watched, "compressed.fdp"))
        wait_for("the removed product to be missing", lambda: products()[packed]["missing"])
        status, body, _ = get(f"/api/v1/data-products/{packed}/export", raw=True)
        assert status == 404 and json.loads(body)["error"]["code"] == "missing", body
        assert get(f"/api/v1/data-products/{packed}")["error"]["code"] == "missing"
        # Changed behind the index: the entry no longer names this content, so it is not read.
        os.remove(os.path.join(watched, "copy", "same.fdp"))
        with open(os.path.join(watched, "Dp_00002576_00000010_00000020.fdp"), "ab") as handle:
            handle.write(b"x")
        assert get(f"/api/v1/data-products/{whole}/export", raw=True)[0] == 404
        wait_for("the changed file to be indexed again", lambda: products()[whole]["missing"] and len(products()) == 5)
        step("a removed product and a changed file are listed as missing and are not read")
    finally:
        server.terminate()
        try:
            server.wait(timeout=20)
        except subprocess.TimeoutExpired:
            server.kill()
        log.close()


print("== dp 2. fgds dp against fixtures/dp")
check_products()
check_cli_cases()
print("== dp 3. index, routes and tab against a running GDS")
check_server()
print("== dp 4. fgds dp against a live fprime-dp on damaged products")
if FPRIME_DP:
    check_against_live_tool()
else:
    print("  skipped: set FPRIME_DP to an fprime-dp of the commit named in fixtures/README.md")
