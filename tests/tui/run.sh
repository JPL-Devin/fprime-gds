#!/usr/bin/env bash
# `fgds tui` typed into through a pseudo-terminal, against the stock Ref.
# Run by scripts/check.sh from the repository root, with FGDS, REF_BIN, DICTIONARY and OUT set.
set -euo pipefail
: "${FGDS:?}" "${REF_BIN:?}" "${DICTIONARY:?}" "${OUT:?}"
mkdir -p "$OUT"

echo "== tui. the three panes against Ref"
python3 tests/tui/check.py --fgds "$FGDS" --ref "$REF_BIN" --dictionary "$DICTIONARY" --out "$OUT"
