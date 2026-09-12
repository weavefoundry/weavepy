#!/bin/bash
set -euo pipefail
export WEAVEPY_STDLIB_CACHE="$PWD/target/performance-stdlib-cache"
export WEAVEPY_BENCH_LAUNCH_CONTEXT="outside-tool-filesystem-sandbox (require_escalated)"
test "$(cat target/weakref-shared-keys-validation-stage.txt)" = done
test "$(cat target/weakref-shared-keys-check-stage.txt)" = done
python3.14 -c 'import json; assert json.load(open("target/weakref-shared-keys-preflight/report.json"))["status"] == "passed"'
test ! -e target/weakref-shared-keys-diagnostic-stage.txt
binary=target/release/weavepy-runtime-weakref-shared-keys
base=target/release/weavepy-runtime-gc-borrowed-handles
vm_stat > target/weakref-shared-keys-diagnostic-vm-before.txt
sysctl hw.memsize vm.swapusage > target/weakref-shared-keys-diagnostic-memory-before.txt
for group in heaps access construction; do
    printf '%s\n' "$group" > target/weakref-shared-keys-diagnostic-stage.txt
    python3.14 target/check_datetime_components_isolated.py --binary "$binary" --previous "$base" --inputs "target/weakref-shared-keys-inputs/$group" --out "target/weakref-shared-keys-diagnostic-$group" --samples 7 > "target/weakref-shared-keys-diagnostic-$group.txt" 2>&1
done
vm_stat > target/weakref-shared-keys-diagnostic-vm-after.txt
sysctl hw.memsize vm.swapusage > target/weakref-shared-keys-diagnostic-memory-after.txt
printf 'done\n' > target/weakref-shared-keys-diagnostic-stage.txt
