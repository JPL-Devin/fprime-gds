#!/usr/bin/env bash
# The checks of the data products part, run by scripts/check.sh with its environment.
#
#   1. unit tests: fixtures, index, fuzz-style robustness (cargo test -p data-products)
#   2. `fgds dp` against every product and mode of fixtures/dp and every command-line case
#   3. the index, the routes and the tab against a running GDS
set -euo pipefail
cd "$(dirname "$0")/../.."
out=${OUT:-target/check/dp}
rm -rf "$out"
mkdir -p "$out"

echo "== dp 1. decoder, validator, index and robustness tests"
cargo test -p data-products --quiet 2>&1 | tee "$out/test.log" | grep -E "^test result|FAILED|panicked|^error" \
    | awk '/^test result: ok/ {n += $4; next} {print; bad = 1} END {if (bad) exit 1; print "  ok: " n " tests passed"}'

fgds=${FGDS:-$PWD/target/debug/fgds}
python3 tests/dp/check.py "$fgds" "$PWD/fixtures/dp" "$(realpath "$out")"
echo "dp: all passed"
