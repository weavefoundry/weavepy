#!/bin/bash
# The default PGO training workload (see tools/pgo/build.sh):
#
#   tools/pgo/train.sh BIN
#
# Runs every tools/pybench benchmark at its default size, for the hot
# paths' frequencies, then the bundled regression tests, for breadth. A
# benchmark that fails is reported and skipped.
#
# Each benchmark runs twice, a short run (the first call and two more) and
# a long one (six more): what the long run executes more often is what the
# benchmark keeps running, as opposed to what its start-up, imports and
# compiles pass through. With WEAVEPY_PGO_RUNS set to a directory, the two
# processes write their profiles there (`NAME.short-PID.profraw`,
# `NAME.long-PID.profraw`), apart from the regression tests' (which merge
# into the instrumented binary's default files), for tools/pgo/order.py to
# tell the two kinds of code apart.
set -uo pipefail

bin=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

for f in tools/pybench/benchmarks/*.py; do
    name=$(basename "$f" .py)
    work=$(grep -oE '^WORK = [0-9]+' "$f" | grep -oE '[0-9]+')
    for run in short:2 long:6; do
        if [ -n "${WEAVEPY_PGO_RUNS:-}" ]; then
            export LLVM_PROFILE_FILE="$WEAVEPY_PGO_RUNS/$name.${run%%:*}-%p.profraw"
        fi
        "$bin" tools/pybench/harness.py "$name" "$work" "${run##*:}" >/dev/null 2>&1 \
            || echo "training run failed: $name" >&2
    done
done
unset LLVM_PROFILE_FILE

"$bin" regrtest --mode subprocess --workers 4 -q >/dev/null 2>&1 \
    || echo "training run failed: regrtest" >&2
