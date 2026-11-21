#!/usr/bin/env bash
# The checks of the CCSDS framing, run by scripts/check.sh from the root of the repository
# with FGDS, REF_BIN, DICTIONARY, VENV and OUT set (and FPRIME, which check.sh requires):
#
#   1. fgds against the stock Ref (CCSDS framing): commands, events, channels, packet metadata
#   2. comparison with the Python GDS on the stock Ref, live and on replayed bytes
#   3. comparison with the Python GDS on every event and channel of the dictionary (no Ref)
#
# check.sh itself runs items 1 and 2 against the Ref built with F Prime framing.
set -euo pipefail
: "${FGDS:?}" "${VENV:?}" "${OUT:?}"
: "${FPRIME:?set FPRIME to an fprime checkout with its fprime-venv}"

# Until framing-ccsds is in the default plugins of fgds, an fgds that has it is built here.
fgds=$FGDS
if ! "$fgds" check --dictionary tests/data/dictionary.json --framing ccsds >/dev/null 2>&1; then
    cargo build --quiet -p framing-ccsds --example fgds-ccsds
    fgds=$PWD/target/debug/examples/fgds-ccsds
fi

echo "== Ref as it is (CCSDS framing)"
ref=$PWD/target/ref-ccsds/TestDeploymentsProject/build-artifacts/Linux/Ref
PATH="$VENV/bin:$PATH" scripts/build-ref.sh "$FPRIME" target/ref-ccsds ccsds
dictionary=$ref/dict/RefTopologyDictionary.json
echo "  ok: ${ref#"$PWD"/}/bin/Ref"

echo "== ccsds 1. fgds against the stock Ref"
python3 tests/ref/check.py "$fgds" "$ref/bin/Ref" "$dictionary" "$OUT/ref" ccsds

echo "== ccsds 2. comparison with the Python GDS on the stock Ref"
PATH="$VENV/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" --ref "$ref/bin/Ref" --framing ccsds \
    --dictionary "$dictionary" --python "$VENV/bin/python" --out "$OUT/compare"

echo "== ccsds 3. comparison with the Python GDS on every event and channel"
PATH="$VENV/bin:$PATH" python3 tests/compare/compare.py --fgds "$fgds" \
    --dictionary "$dictionary" --python "$VENV/bin/python" --out "$OUT/synthetic"

echo "ccsds: all passed"
