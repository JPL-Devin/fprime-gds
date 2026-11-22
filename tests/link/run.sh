#!/usr/bin/env bash
# The first bytes of a connection (README.md). Run by scripts/check.sh, from the
# repository root, with FGDS, REF_BIN, DICTIONARY, VENV and OUT set.
set -euo pipefail
: "${FGDS:?}" "${REF_BIN:?}" "${DICTIONARY:?}" "${OUT:?}"
python3 "$(dirname "$0")/check.py" "$FGDS" "$REF_BIN" "$DICTIONARY" "$OUT"
