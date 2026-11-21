#!/usr/bin/env bash
# File uplink and downlink against Ref, compared with the Python GDS (tests/files/README.md).
set -euo pipefail
PATH="$VENV/bin:$PATH" "$VENV/bin/python" tests/files/check.py --fgds "$FGDS" --ref "$REF_BIN" \
    --dictionary "$DICTIONARY" --out "$OUT"
