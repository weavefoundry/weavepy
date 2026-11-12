#!/bin/bash
# The default PGO training workload (see tools/pgo/build.sh):
#
#   tools/pgo/train.sh BIN
#
# Runs every tools/pybench benchmark once at its default size, for the
# hot paths' frequencies, then the bundled regression tests, for breadth.
# A benchmark that fails is reported and skipped.
#
# With WEAVEPY_PGO_RUNS set to a directory, each benchmark's process writes
# its profile there, one file a process, apart from the regression tests'
# (which merge into the instrumented binary's default files): the order
# file is made from what single processes run (tools/pgo/order.py).
set -uo pipefail

bin=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

for f in tools/pybench/benchmarks/*.py; do
    name=$(basename "$f" .py)
    work=$(grep -oE '^WORK = [0-9]+' "$f" | grep -oE '[0-9]+')
    if [ -n "${WEAVEPY_PGO_RUNS:-}" ]; then
        export LLVM_PROFILE_FILE="$WEAVEPY_PGO_RUNS/$name-%p.profraw"
    fi
    "$bin" tools/pybench/harness.py "$name" "$work" 1 >/dev/null 2>&1 \
        || echo "training run failed: $name" >&2
done
unset LLVM_PROFILE_FILE

"$bin" regrtest --mode subprocess --workers 4 -q >/dev/null 2>&1 \
    || echo "training run failed: regrtest" >&2
