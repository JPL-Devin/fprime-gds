#!/usr/bin/env bash
# The checks of the Python client and of the online subcommands (README.md).
# Same environment as scripts/check.sh: FPRIME, optionally FPRIME_VENV. Ref is expected
# under target/ref (scripts/check.sh builds it); it is built here when missing.
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${FPRIME:?set FPRIME to an fprime checkout with its fprime-venv}"
venv=${FPRIME_VENV:-$FPRIME/fprime-venv}
out=$PWD/target/check
mkdir -p "$out"

ref=$PWD/target/ref/TestDeploymentsProject/build-artifacts/Linux/Ref
if [ ! -x "$ref/bin/Ref" ]; then
    PATH="$venv/bin:$PATH" scripts/build-ref.sh "$FPRIME" target/ref
fi
dictionary=$ref/dict/RefTopologyDictionary.json

# fgds with the online subcommands: the stock binary once OnlinePlugin is among its
# defaults, the example binary of plugins/online until then.
cargo build --quiet -p fgds
fgds=$PWD/target/debug/fgds
if ! "$fgds" events --help >/dev/null 2>&1; then
    cargo build --quiet -p online --example fgds-online
    fgds=$PWD/target/debug/examples/fgds-online
fi
echo "  fgds: $fgds"

echo "== cli 1. Python client, without a GDS"
PYTHONPATH=$PWD/python "$venv/bin/python" -m pytest -q -p no:fprime_test_api -p no:cacheprovider python/tests/unit

echo "== cli 2. Ref integration tests through fgds_client"
"$venv/bin/python" tests/cli/ref_int.py --fgds "$fgds" --ref "$ref/bin/Ref" --dictionary "$dictionary" --out "$out/ref-int"

echo "== cli 3. fgds subcommands against fprime-cli"
python3 tests/cli/check.py --fgds "$fgds" --ref "$ref/bin/Ref" --dictionary "$dictionary" \
    --venv-bin "$venv/bin" --out "$out/cli"
