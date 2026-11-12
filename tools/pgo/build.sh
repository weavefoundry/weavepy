#!/bin/bash
# Build a profile-guided (PGO) release `weavepy`.
#
#   tools/pgo/build.sh [TRAIN_COMMAND...]
#
# 1. Build an instrumented release binary into target/pgo/gen.
# 2. Run the training workload on it (tools/pgo/train.sh by default, or
#    TRAIN_COMMAND with the instrumented binary's path appended).
# 3. Merge the raw profiles into target/pgo/weavepy.profdata, and write
#    the linker order file target/pgo/weavepy.order from what the
#    training ran (tools/pgo/order.py).
# 4. Build the optimized binary into target/pgo/use, linked in that
#    order, with the code the training never reached split out of the
#    functions it did; it is printed at the end
#    (target/pgo/use/release/weavepy).
#
# Needs the toolchain's llvm-profdata: `rustup component add llvm-tools`.
#
# The C dependencies (mimalloc, expat, lzma, sqlite) aren't instrumented:
# the `cc` crate would otherwise pass `-Cprofile-generate` on to clang,
# whose profile runtime crashes against rustc's at thread start.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

host=$(rustc -vV | sed -n 's/^host: //p')
profdata="$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-profdata"
if [ ! -x "$profdata" ]; then
    echo "llvm-profdata not found; run: rustup component add llvm-tools" >&2
    exit 1
fi

data="$root/target/pgo/data"
merged="$root/target/pgo/weavepy.profdata"
rm -rf "$data"
mkdir -p "$data"

echo "== instrumented build"
CFLAGS="-fno-profile-generate" \
RUSTFLAGS="-Cprofile-generate=$data -Cllvm-args=-disable-vp=true" \
    cargo build --release --target-dir target/pgo/gen -p weavepy-cli --bin weavepy

# The build scripts ran instrumented too: only the training counts.
rm -rf "$data"
mkdir -p "$data"

echo "== training"
gen="$root/target/pgo/gen/release/weavepy"
runs="$data/runs"
mkdir -p "$runs"
export WEAVEPY_PGO_RUNS="$runs"
if [ $# -gt 0 ]; then
    "$@" "$gen"
else
    "$root/tools/pgo/train.sh" "$gen"
fi

echo "== merging"
shopt -s nullglob
raw=("$data"/*.profraw "$runs"/*.profraw)
"$profdata" merge -o "$merged" "${raw[@]}"
# One profile per benchmark process, for the order file's hot group (a
# training command that left none gives an order from the merged counts).
for f in "$runs"/*.profraw; do
    "$profdata" merge -o "${f%.profraw}.profdata" "$f"
done
order="$root/target/pgo/weavepy.order"
python3 "$root/tools/pgo/order.py" "$profdata" "$merged" \
    "$root/crates/weavepy-cli/weavepy.order" "$order" --runs "$runs"

echo "== optimized build"
# Code no training run reached leaves the functions that did run, for a
# section of its own behind everything else (`-hot-cold-split`): a
# function's pages then hold what runs, not what might.
CFLAGS="-fno-profile-instr-use" \
WEAVEPY_ORDER_FILE="$order" \
RUSTFLAGS="-Cprofile-use=$merged -Cllvm-args=-pgo-warn-missing-function=false ${WEAVEPY_PGO_RUSTFLAGS:-}" \
    cargo build --release --target-dir target/pgo/use -p weavepy-cli --bin weavepy

echo "$root/target/pgo/use/release/weavepy"
