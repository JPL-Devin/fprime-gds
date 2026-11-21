#!/usr/bin/env bash
# The checks of the data products part, run by scripts/check.sh with its environment.
#
#   1. unit tests: fixtures, index, fuzz-style robustness (cargo test -p data-products)
#   2. `fgds dp` against every product and mode of fixtures/dp and every command-line case
#   3. the index, the routes and the tab against a running GDS
#   4. optional: `fgds dp` against a live `fprime-dp` on damaged products
#      (FPRIME_DP=<fprime-dp of the commit in fixtures/README.md>)
set -euo pipefail
cd "$(dirname "$0")/../.."
out=${OUT:-target/check/dp}
rm -rf "$out"
mkdir -p "$out"

echo "== dp 1. decoder, validator, index and robustness tests"
cargo test -p data-products --quiet 2>&1 | tee "$out/test.log" | grep -E "^test result|FAILED|panicked|^error" \
    | awk '/^test result: ok/ {n += $4; next} {print; bad = 1} END {if (bad) exit 1; print "  ok: " n " tests passed"}'

# The stock binary once the plugin is in its default list; until then the same GDS with
# the plugin added (plugins/data-products/examples/fgds-dp.rs).
fgds=${FGDS:-$PWD/target/debug/fgds}
if ! "$fgds" dp decode --help >/dev/null 2>&1; then
    cargo build -p data-products --example fgds-dp --quiet
    fgds=$PWD/target/debug/examples/fgds-dp
fi
echo "  binary: $fgds"
python3 tests/dp/check.py "$fgds" "$PWD/fixtures/dp" "$(realpath "$out")" ${FPRIME_DP:+--fprime-dp "$FPRIME_DP"}
echo "dp: all passed"
