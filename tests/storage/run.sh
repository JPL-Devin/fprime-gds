#!/usr/bin/env bash
# SQLite storage against Ref: a killed GDS, a restart, a changed dictionary, a replay
# of the database (STO-4, STO-5, STO-11, STO-12, STO-14).
set -euo pipefail
python3 tests/storage/check.py "$FGDS" "$REF_BIN" "$DICTIONARY" "$OUT"
