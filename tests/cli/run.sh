#!/usr/bin/env bash
# The checks of the Python client and of the online subcommands (README.md).
# Run by scripts/check.sh from the repository root, with FGDS, REF_BIN, DICTIONARY, VENV
# and OUT set.
set -euo pipefail
: "${FGDS:?}" "${REF_BIN:?}" "${DICTIONARY:?}" "${VENV:?}" "${OUT:?}"
mkdir -p "$OUT"

# fgds with the online subcommands: the stock binary once OnlinePlugin is among its
# defaults, the example binary of plugins/online until then.
fgds=$FGDS
if ! "$fgds" events --help >/dev/null 2>&1; then
    cargo build --quiet -p online --example fgds-online
    fgds=$PWD/target/debug/examples/fgds-online
fi
echo "  fgds: $fgds"

echo "== cli 1. Python client, without a GDS"
PYTHONPATH=$PWD/python "$VENV/bin/python" -m pytest -q -p no:fprime_test_api -p no:cacheprovider python/tests/unit

echo "== cli 2. Ref integration tests through fgds_client"
"$VENV/bin/python" tests/cli/ref_int.py --fgds "$fgds" --ref "$REF_BIN" --dictionary "$DICTIONARY" --out "$OUT/ref-int"

echo "== cli 3. fgds subcommands against fprime-cli"
python3 tests/cli/check.py --fgds "$fgds" --ref "$REF_BIN" --dictionary "$DICTIONARY" \
    --venv-bin "$VENV/bin" --out "$OUT/cli"
