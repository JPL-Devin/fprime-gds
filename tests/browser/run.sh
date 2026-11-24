#!/usr/bin/env bash
# The browser checks (README.md): the page against a running Ref. Run by scripts/check.sh, from the
# repository root, with FGDS, REF_BIN, DICTIONARY, VENV and OUT set.
#
# Playwright and its Chromium are installed on the first run, in target/browser-venv.
set -euo pipefail
: "${FGDS:?}" "${REF_BIN:?}" "${DICTIONARY:?}" "${OUT:?}"
here=$(dirname "$0")
tools=target/browser-venv
if [ ! -x "$tools/bin/playwright" ]; then
    python3 -m venv "$tools"
    "$tools/bin/pip" install --quiet playwright==1.60.0
fi
"$tools/bin/playwright" install chromium >/dev/null

# The stock binary; the binary with a tab that fails is used for that one check only.
cargo build --quiet -p browser-gds

"$tools/bin/python" "$here/ui.py" "$FGDS" "$PWD/target/debug/browser-gds" "$REF_BIN" "$DICTIONARY" "$OUT/ui"
