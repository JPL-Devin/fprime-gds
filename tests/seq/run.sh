#!/usr/bin/env bash
# Checks of the sequence compiler and the parameter tools against the Python tools
# (fprime-seqgen, fprime-prm-write, fprime-prm-decode). Run by scripts/check.sh from the
# repository root with FGDS, DICTIONARY, VENV and OUT set.
set -euo pipefail
: "${FGDS:?}" "${DICTIONARY:?}" "${VENV:?}" "${OUT:?}"
mkdir -p "$OUT"


# Both parts compare binaries with those of `fprime-seqgen` and `fprime-prm-write` of the Python GDS.
if [ -z "${COMPARE:-}" ]; then
    echo "  skipped: set COMPARE=1 to compare with the Python GDS (the fixture binaries are checked by cargo test)"
    exit 0
fi

echo "  -- generated sequences, fixture dictionary"
"$VENV/bin/python" tests/seq/differential.py --fgds "$FGDS" --fixtures fixtures --out "$OUT/differential" \
    --count "${SEQ_COUNT:-1500}"

echo "  -- sequences and parameters of Ref"
PATH="$VENV/bin:$PATH" "$VENV/bin/python" tests/seq/ref.py --fgds "$FGDS" --dictionary "$DICTIONARY" --out "$OUT/ref"
