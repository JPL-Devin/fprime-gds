#!/usr/bin/env bash
# Every check of this repository, the same locally and in the workflow.
#
#   1. format, lints and tests
#   2. fgds against a running Ref: commands, events, channels, WebSocket
#   3. the example project binary
#   4. every tests/*/run.sh
#   5. with COMPARE=1 only: comparison with the Python GDS of the bytes sent to the flight software and
#      of the data decoded, live and on replayed bytes (tests/compare/README.md)
#
# Items 2 and 4 need F Prime: set FPRIME to a checkout of fprime that has its virtual environment in
# fprime-venv (fprime-util, to build Ref). Item 5 also needs fprime-gds installed there. Ref is built
# from copies of the checkout, on the first run only: as it is (CCSDS framing, the main build, under
# target/ref-ccsds) and with F Prime framing (the second profile, under target/ref).
set -euo pipefail
cd "$(dirname "$0")/.."
: "${FPRIME:?set FPRIME to an fprime checkout with its fprime-venv}"
venv=${FPRIME_VENV:-$FPRIME/fprime-venv}
out=target/check
rm -rf "$out"
mkdir -p "$out"

echo "== 1. format, lints, tests"
if grep -L '^workspace = true' $(grep -l '^\[lints\]' crates/*/Cargo.toml plugins/*/Cargo.toml examples/*/Cargo.toml) | grep .; then
    echo "these crates do not take the workspace lints (unsafe_code = forbid)"; exit 1
fi
test "$(ls crates/*/Cargo.toml plugins/*/Cargo.toml examples/*/Cargo.toml | wc -l)" = \
     "$(grep -l '^\[lints\]' crates/*/Cargo.toml plugins/*/Cargo.toml examples/*/Cargo.toml | wc -l)"
grep -q '^unsafe_code = "forbid"' Cargo.toml
echo "  ok: every crate forbids unsafe"
# MNT-1: plugins depend on the core crates only, never on another plugin.
cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
plugins = {p["name"] for p in packages if "/plugins/" in p["manifest_path"]}
bad = [(p["name"], d["name"]) for p in packages if p["name"] in plugins
       for d in p["dependencies"] if d["name"] in plugins and d.get("kind") != "dev"]
for name, dependency in bad:
    print(f"plugin {name} depends on plugin {dependency}")
sys.exit(1 if bad else 0)'
echo "  ok: no plugin depends on another plugin"
cargo fmt --check
echo "  ok: cargo fmt --check"
cargo clippy --workspace --all-targets --quiet -- -D warnings
echo "  ok: cargo clippy -D warnings"
cargo test --workspace --quiet 2>&1 | tee "$out/test.log" | grep -E "^test result|FAILED|panicked|^error" \
    | awk '/^test result: ok/ {n += $4; next} {print; bad = 1} END {if (bad) exit 1; print "  ok: cargo test, " n " tests passed"}'
cargo build --workspace --quiet

# Each Ref is built once, on the first run, and every check below uses these two builds.
echo "== Ref as it is (CCSDS framing), and Ref with F Prime framing"
ref=$PWD/target/ref-ccsds/TestDeploymentsProject/build-artifacts/Linux/Ref
ref_fprime=$PWD/target/ref/TestDeploymentsProject/build-artifacts/Linux/Ref
PATH="$venv/bin:$PATH" scripts/build-ref.sh "$FPRIME" target/ref-ccsds ccsds
PATH="$venv/bin:$PATH" scripts/build-ref.sh "$FPRIME" target/ref fprime
dictionary=$ref/dict/RefTopologyDictionary.json
dictionary_fprime=$ref_fprime/dict/RefTopologyDictionary.json
fgds=$PWD/target/debug/fgds
echo "  ok: ${ref#"$PWD"/}/bin/Ref, ${ref_fprime#"$PWD"/}/bin/Ref"

echo "== 2. fgds against Ref"
python3 tests/ref/check.py "$fgds" "$ref/bin/Ref" "$dictionary" "$PWD/$out/ref" ccsds
echo "== 2. fgds against Ref with F Prime framing"
python3 tests/ref/check.py "$fgds" "$ref_fprime/bin/Ref" "$dictionary_fprime" "$PWD/$out/ref-fprime" fprime

echo "== 3. example project binary"
python3 examples/custom-gds/check.py target/debug tests/data/dictionary.json "$PWD/$out/example"

# Each part of the GDS adds its own checks as tests/<name>/run.sh, without editing this
# file. A script runs from the repository root with FGDS, REF_BIN, DICTIONARY, VENV and
# OUT (its own directory for output) set, and fails with a non-zero exit code. REF_BIN and
# DICTIONARY are the stock Ref (CCSDS framing, the default of fgds). With COMPARE set, a script
# also runs its comparison with the Python GDS.
for script in tests/*/run.sh; do
    [ -e "$script" ] || continue
    name=$(basename "$(dirname "$script")")
    echo "== $script"
    mkdir -p "$out/$name"
    FGDS="$fgds" REF_BIN="$ref/bin/Ref" DICTIONARY="$dictionary" VENV="$venv" \
        OUT="$PWD/$out/$name" bash "$script"
done

if [ -n "${COMPARE:-}" ]; then
    echo "== 5. comparison with the Python GDS"
    PATH="$venv/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" --ref "$ref/bin/Ref" --framing ccsds \
        --dictionary "$dictionary" --python "$venv/bin/python" --out "$PWD/$out/compare"
    echo "== 5. comparison with the Python GDS, Ref with F Prime framing"
    PATH="$venv/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" --ref "$ref_fprime/bin/Ref" --framing fprime \
        --dictionary "$dictionary_fprime" --python "$venv/bin/python" --out "$PWD/$out/compare-fprime"
    echo "== 5. comparison with the Python GDS on every event and channel of the dictionary"
    PATH="$venv/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" \
        --dictionary "$dictionary" --python "$venv/bin/python" --out "$PWD/$out/synthetic"
fi

echo "== nothing left behind"
# Databases and logs go to the output directory of each check, never into the tree.
left=$(git status --porcelain --ignored | grep -v -e '^!! target/$' -e '__pycache__/$' || true)
if [ -n "$left" ]; then
    echo "files left in the repository tree:"; echo "$left"; exit 1
fi
echo "  ok: no file left in the repository tree"
# A process started by a check and still running would disturb the next run.
left=$(pgrep -af "$PWD/target/|fprime_gds|fprime-gds" | grep -v -e "scripts/check.sh" -e pgrep || true)
if [ -n "$left" ]; then
    echo "processes left running:"; echo "$left"; exit 1
fi
echo "  ok: no process left running"

echo "check: all passed"
