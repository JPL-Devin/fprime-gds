#!/usr/bin/env bash
# The checks of the CCSDS framing, run by scripts/check.sh with its environment (FPRIME,
# FPRIME_VENV) from the root of the repository:
#
#   1. fgds against the stock Ref (CCSDS framing): commands, events, channels, packet metadata
#   2. comparison with the Python GDS on the stock Ref, live and on replayed bytes
#   3. comparison with the Python GDS on every event and channel of the dictionary (no Ref)
#
# check.sh runs items 1 and 2 against the Ref built with F Prime framing (target/ref).
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${FPRIME:?set FPRIME to an fprime checkout with its fprime-venv}"
venv=${FPRIME_VENV:-$FPRIME/fprime-venv}
out=$PWD/target/check/ccsds
rm -rf "$out"
mkdir -p "$out"

# fgds with the CCSDS plugin, until the plugin is in the default list of fgds.
cargo build --quiet -p fgds -p framing-ccsds --examples
fgds=$PWD/target/debug/examples/fgds-ccsds

echo "== Ref as it is (CCSDS framing)"
ref=$PWD/target/ref-ccsds/TestDeploymentsProject/build-artifacts/Linux/Ref
PATH="$venv/bin:$PATH" scripts/build-ref.sh "$FPRIME" target/ref-ccsds ccsds
dictionary=$ref/dict/RefTopologyDictionary.json
echo "  ok: ${ref#"$PWD"/}/bin/Ref"

echo "== ccsds 1. fgds against the stock Ref"
python3 tests/ref/check.py "$fgds" "$ref/bin/Ref" "$dictionary" "$out/ref" ccsds

echo "== ccsds 2. comparison with the Python GDS on the stock Ref"
PATH="$venv/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" --ref "$ref/bin/Ref" --framing ccsds \
    --dictionary "$dictionary" --python "$venv/bin/python" --out "$out/compare"

echo "== ccsds 3. comparison with the Python GDS on every event and channel"
PATH="$venv/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" \
    --dictionary "$dictionary" --python "$venv/bin/python" --out "$out/synthetic"

echo "ccsds: all passed"
