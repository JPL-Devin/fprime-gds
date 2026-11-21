#!/usr/bin/env bash
# The page shell under a failing tab module and a stream that ends (UI-3). Needs node.
set -euo pipefail
if ! command -v node >/dev/null; then
    echo "  skipped: node is not installed"
    exit 0
fi
node tests/ui/shell.test.mjs crates/fgds-shell/ui/shell.js
