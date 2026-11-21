#!/usr/bin/env bash
# The browser checks (README.md): the page against a running Ref, then the Events and
# Channels tables against those of the Python GDS page. Run by scripts/check.sh, from the
# repository root, with REF_BIN, DICTIONARY, VENV and OUT set.
#
# Playwright and its Chromium are installed on the first run, in target/browser-venv.
set -euo pipefail
: "${REF_BIN:?}" "${DICTIONARY:?}" "${VENV:?}" "${OUT:?}"
here=$(dirname "$0")
tools=target/browser-venv
if [ ! -x "$tools/bin/playwright" ]; then
    python3 -m venv "$tools"
    "$tools/bin/pip" install --quiet playwright==1.60.0
fi
"$tools/bin/playwright" install chromium >/dev/null

# The stock plugins and the tab plugins that are not in the default list yet.
cargo build --quiet -p browser-gds
gds=$PWD/target/debug/browser-gds

"$tools/bin/python" "$here/ui.py" "$gds" "$REF_BIN" "$DICTIONARY" "$OUT/ui"
"$tools/bin/python" "$here/parity.py" "$gds" "$REF_BIN" "$DICTIONARY" "$VENV/bin/fprime-gds" "$OUT/parity"
