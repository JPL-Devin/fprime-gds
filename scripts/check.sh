#!/usr/bin/env bash
# Every check of this repository, the same locally and in the workflow.
#
#   1. format, lints and tests
#   2. fgds against a running Ref: commands, events, channels, WebSocket
#   3. comparison with the Python GDS, live and on replayed bytes (tests/compare/README.md)
#   4. the example project binary
#   5. every tests/*/run.sh
#
# Items 2 and 3 need F Prime: set FPRIME to a checkout of fprime that has its virtual
# environment in fprime-venv (fprime-util and fprime-gds installed). Ref is built from a
# copy of it under target/ref, on the first run only.
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

echo "== Ref with F Prime framing"
ref=target/ref/TestDeploymentsProject/build-artifacts/Linux/Ref
PATH="$venv/bin:$PATH" scripts/build-ref.sh "$FPRIME" target/ref
dictionary=$PWD/$ref/dict/RefTopologyDictionary.json
echo "  ok: $ref/bin/Ref"

echo "== 2. fgds against Ref"
python3 tests/ref/check.py "$PWD/target/debug/fgds" "$PWD/$ref/bin/Ref" "$dictionary" "$PWD/$out/ref"

echo "== 3. comparison with the Python GDS"
PATH="$venv/bin:$PATH" python3 tests/compare/compare.py --fgds "$PWD/target/debug/fgds" --ref "$PWD/$ref/bin/Ref" \
    --dictionary "$dictionary" --python "$venv/bin/python" --out "$PWD/$out/compare"

echo "== 4. example project binary"
python3 examples/custom-gds/check.py target/debug tests/data/dictionary.json

# Each part of the GDS adds its own checks as tests/<name>/run.sh, without editing this
# file. A script runs from the repository root with FGDS, REF_BIN, DICTIONARY, VENV and
# OUT (its own directory for output) set, and fails with a non-zero exit code.
for script in tests/*/run.sh; do
    [ -e "$script" ] || continue
    name=$(basename "$(dirname "$script")")
    echo "== $script"
    mkdir -p "$out/$name"
    FGDS="$PWD/target/debug/fgds" REF_BIN="$PWD/$ref/bin/Ref" DICTIONARY="$dictionary" VENV="$venv" \
        OUT="$PWD/$out/$name" bash "$script"
done

echo "check: all passed"
