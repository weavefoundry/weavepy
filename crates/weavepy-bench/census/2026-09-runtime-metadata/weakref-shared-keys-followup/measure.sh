#!/bin/bash
set -euo pipefail
export WEAVEPY_BENCH_LAUNCH_CONTEXT="outside-tool-filesystem-sandbox (require_escalated)"
export WEAVEPY_STDLIB_CACHE="$PWD/target/performance-stdlib-cache"
test "$(cat target/weakref-shared-keys-validation-stage.txt)" = done
test "$(cat target/weakref-shared-keys-check-stage.txt)" = done
test "$(cat target/weakref-shared-keys-diagnostic-stage.txt)" = done
test ! -e target/weakref-shared-keys-full
mkdir target/weakref-shared-keys-full
base=target/release/weavepy-runtime-gc-traversal-lists
previous=target/release/weavepy-runtime-gc-borrowed-handles
binary=target/release/weavepy-runtime-weakref-shared-keys
vm_stat > target/weakref-shared-keys-full/vm-before.txt
sysctl hw.memsize vm.swapusage > target/weakref-shared-keys-full/memory-before.txt
printf 'startup and imports\n' > target/weakref-shared-keys-full/stage.txt
python3.14 target/shared_slot_keys_startup_probes.py --base "$base" --previous "$previous" --new "$binary" --samples 31 --out target/weakref-shared-keys-full/startup.json > target/weakref-shared-keys-full/startup.txt 2>&1
printf 'full 24-workload census\n' > target/weakref-shared-keys-full/stage.txt
python3.14 tools/bench_compare.py --base "$base" --previous "$previous" --new "$binary" --samples 5 --frozen-cache-root target/weakref-shared-keys-full/frozen --out target/weakref-shared-keys-full/suite.json > target/weakref-shared-keys-full/suite.txt 2>&1
vm_stat > target/weakref-shared-keys-full/vm-after.txt
sysctl hw.memsize vm.swapusage > target/weakref-shared-keys-full/memory-after.txt
printf 'done\n' > target/weakref-shared-keys-full/stage.txt
