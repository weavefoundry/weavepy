#!/bin/bash
# The default PGO training workload (see tools/pgo/build.sh):
#
#   tools/pgo/train.sh BIN
#
# Runs every tools/pybench benchmark once at its default size, for the
# hot paths' frequencies, then the bundled regression tests, for breadth.
# A benchmark that fails is reported and skipped.
set -uo pipefail

bin=$1
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

for f in tools/pybench/benchmarks/*.py; do
    name=$(basename "$f" .py)
    work=$(grep -oE '^WORK = [0-9]+' "$f" | grep -oE '[0-9]+')
    "$bin" tools/pybench/harness.py "$name" "$work" 1 >/dev/null 2>&1 \
        || echo "training run failed: $name" >&2
done

"$bin" regrtest --mode subprocess --workers 4 -q >/dev/null 2>&1 \
    || echo "training run failed: regrtest" >&2
