#!/usr/bin/env bash
# File uplink and downlink against Ref, compared with the Python GDS (tests/files/README.md).
set -euo pipefail
PATH="$VENV/bin:$PATH" "$VENV/bin/python" tests/files/check.py --fgds "$FGDS" --ref "$REF_BIN" \
    --dictionary "$DICTIONARY" --out "$OUT"
PATH="$VENV/bin:$PATH" "$VENV/bin/python" tests/files/repeat.py --fgds "$FGDS" --ref "$REF_BIN" \
    --dictionary "$DICTIONARY" --out "$OUT"
# The largest chunk size of each framing (the F´ Ref of scripts/check.sh, when it is built).
PATH="$VENV/bin:$PATH" "$VENV/bin/python" tests/files/chunk.py --fgds "$FGDS" --ref "$REF_BIN" \
    --dictionary "$DICTIONARY" --out "$OUT"
ref_fprime=$PWD/target/ref/TestDeploymentsProject/build-artifacts/Linux/Ref
if [ -x "$ref_fprime/bin/Ref" ]; then
    PATH="$VENV/bin:$PATH" "$VENV/bin/python" tests/files/chunk.py --fgds "$FGDS" --ref "$ref_fprime/bin/Ref" \
        --dictionary "$ref_fprime/dict/RefTopologyDictionary.json" --framing fprime --out "$OUT"
fi
