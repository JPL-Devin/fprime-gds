#!/usr/bin/env bash
# Regenerate every fixture from a checkout of the Python GDS (JPL-Devin/fprime-gds, branch devel).
#
# Usage: fixtures/tools/regenerate.sh <python gds checkout> [fixtures dir]
#
# The checkout must have the Python GDS installed in a virtual environment:
#   python3 -m venv <checkout>/.venv && <checkout>/.venv/bin/pip install -e <checkout> pytest
# Set PYTHON to use another interpreter.
set -euo pipefail

GDS="$(cd "$1" && pwd)"
TOOLS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURES="$(cd "${2:-$TOOLS/..}" && pwd)"
PYTHON="${PYTHON:-$GDS/.venv/bin/python}"
TESTS="$GDS/test/fprime_gds/common"
export PYTHONHASHSEED="${PYTHONHASHSEED:-0}"

# 1. Inputs copied unchanged from the Python repository
rm -rf "$FIXTURES/dp" "$FIXTURES/sequences" "$FIXTURES/dictionary" "$FIXTURES/vectors"
mkdir -p "$FIXTURES/dp" "$FIXTURES/sequences" "$FIXTURES/dictionary" "$FIXTURES/vectors"
cp -r "$TESTS/dp/test_dp_data" "$FIXTURES/dp/test_dp_data"
cp -r "$TESTS/tools/input" "$TESTS/tools/expected" "$TESTS/tools/resources" "$FIXTURES/sequences/"
cp "$TESTS/loaders/resources/RefTopologyDictionary.json" "$FIXTURES/dictionary/"

# 2. Expected outputs of fprime-dp
"$PYTHON" "$TOOLS/gen_dp_expected.py" "$FIXTURES"

# 3. Byte vectors and the sequence manifest
"$PYTHON" "$TOOLS/gen_vectors.py" "$FIXTURES" > /dev/null

# 4. CCSDS vectors, recorded while the Python CCSDS tests run
(cd "$GDS" && PYTHONPATH="$TOOLS" CCSDS_RECORDER_OUTPUT="$FIXTURES/vectors/ccsds.json" \
    "$PYTHON" -m pytest -q -p ccsds_recorder -p no:cacheprovider test/fprime_gds/common/communication/ccsds)

# 5. Check the data product expectations against what the Python tests assert
"$PYTHON" "$TOOLS/verify_dp.py" "$FIXTURES" > /dev/null

"$PYTHON" "$TOOLS/versions.py" "$GDS" > "$FIXTURES/VERSIONS.txt"
