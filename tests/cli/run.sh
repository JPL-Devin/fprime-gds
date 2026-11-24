#!/usr/bin/env bash
# The checks of the Python client and of the online subcommands (README.md).
# Run by scripts/check.sh from the repository root, with FGDS, REF_BIN, DICTIONARY, VENV
# and OUT set.
set -euo pipefail
: "${FGDS:?}" "${REF_BIN:?}" "${DICTIONARY:?}" "${VENV:?}" "${OUT:?}"
mkdir -p "$OUT"

fgds=$FGDS

echo "== cli 1. Python client, without a GDS"
PYTHONPATH=$PWD/python "$VENV/bin/python" -m pytest -q -p no:fprime_test_api -p no:cacheprovider python/tests/unit

echo "== cli 2. Ref integration tests through fgds_client"
# The test files of the fprime checkout, unchanged. test_fpy needs fprime-fpyc with llvmlite, which the
# environment does not have: it fails before reaching any GDS, with the Python GDS too.
"$VENV/bin/python" tests/cli/ref_int.py --fgds "$fgds" --ref "$REF_BIN" --dictionary "$DICTIONARY" --out "$OUT/ref-int" \
    --tests "${FPRIME:?}/TestDeploymentsProject/Ref/test/int" --deselect test_fpy.py::test_fpy_sequence

echo "== cli 3. fgds subcommands against Ref"
python3 tests/cli/check.py --fgds "$fgds" --ref "$REF_BIN" --dictionary "$DICTIONARY" --out "$OUT/cli"

echo "== cli 4. fgds check and fgds run refuse the same plugin option values"
python3 tests/cli/options.py --fgds "$fgds" --dictionary "$DICTIONARY" --out "$OUT/options"

echo "== cli 5. no text output carries a control character of the data"
"$VENV/bin/python" tests/cli/visible.py --fgds "$fgds" --ref "$REF_BIN" --dictionary "$DICTIONARY" --out "$OUT/visible"
