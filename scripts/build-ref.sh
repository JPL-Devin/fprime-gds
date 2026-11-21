#!/usr/bin/env bash
# Build the Ref deployment, for the checks against flight software.
#
# Usage: build-ref.sh <fprime checkout> <work directory> [ccsds|fprime]
# Needs fprime-util on the PATH (the fprime virtual environment). Prints nothing on
# success; the result is <work directory>/TestDeploymentsProject/build-artifacts/Linux/Ref.
#
# The checkout is copied and the copy is built; the checkout itself is not touched.
#   ccsds (default): Ref as it is, with the ComCcsds subtopology; what users run.
#   fprime: the copy is switched to the ComFprime subtopology, the second framing
#           configuration the checks run against.
set -euo pipefail
fprime=$(cd "$1" && pwd)
mkdir -p "$2"
work=$(cd "$2" && pwd)
framing=${3:-ccsds}
top="$work/TestDeploymentsProject/Ref/Top"
artifacts="$work/TestDeploymentsProject/build-artifacts/Linux/Ref"

if [ -x "$artifacts/bin/Ref" ] && [ -f "$artifacts/dict/RefTopologyDictionary.json" ]; then
    exit 0
fi

rsync -a --delete --exclude .git --exclude 'build-*' --exclude '*-venv' "$fprime/" "$work/"

case "$framing" in
ccsds) ;;
fprime)
    sed -i 's/ComCcsds/ComFprime/g' "$top/topology.fpp" "$top/RefTopologyDefs.hpp" "$top/RefPackets.fppi"
    # Ports that only the CCSDS subtopology has.
    sed -i '/Subtopology\.aggregatorTimeout/d; /ComFprime\.Subtopology\.bufferManagerSchedIn/d' "$top/topology.fpp"
    # The topology code names the queue port enumerations without their namespace.
    sed -i '/ComFprime\/SubtopologyTopologyDefs.hpp/a\
#include "Svc/Subtopologies/ComFprime/Ports_ComPacketQueueEnumAc.hpp"\
#include "Svc/Subtopologies/ComFprime/Ports_ComBufferQueueEnumAc.hpp"\
using namespace ComFprime;' "$top/RefTopologyDefs.hpp"
    ;;
*)
    echo "build-ref.sh: framing is ccsds or fprime, not \"$framing\"" >&2
    exit 2
    ;;
esac

cd "$work/TestDeploymentsProject/Ref"
fprime-util generate > "$work/ref-build.log" 2>&1 || { tail -40 "$work/ref-build.log" >&2; exit 1; }
fprime-util build -j"$(nproc)" >> "$work/ref-build.log" 2>&1 || { tail -40 "$work/ref-build.log" >&2; exit 1; }
test -x "$artifacts/bin/Ref"
