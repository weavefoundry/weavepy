#!/bin/bash
# Build a profile-guided (PGO) release `weavepy`.
#
#   tools/pgo/build.sh [TRAIN_COMMAND...]
#
# 1. Build an instrumented release binary into target/pgo/gen.
# 2. Run the training workload on it (tools/pgo/train.sh by default, or
#    TRAIN_COMMAND with the instrumented binary's path appended).
# 3. Merge the raw profiles into target/pgo/weavepy.profdata.
# 4. Build the optimized binary into target/pgo/use; it is printed at the
#    end (target/pgo/use/release/weavepy).
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
    cargo build --release --target-dir target/pgo/gen -p weavepy-cli

# The build scripts ran instrumented too: only the training counts.
rm -rf "$data"
mkdir -p "$data"

echo "== training"
gen="$root/target/pgo/gen/release/weavepy"
if [ $# -gt 0 ]; then
    "$@" "$gen"
else
    "$root/tools/pgo/train.sh" "$gen"
fi

echo "== merging"
"$profdata" merge -o "$merged" "$data"/*.profraw

echo "== optimized build"
CFLAGS="-fno-profile-instr-use" \
RUSTFLAGS="-Cprofile-use=$merged -Cllvm-args=-pgo-warn-missing-function=false" \
    cargo build --release --target-dir target/pgo/use -p weavepy-cli

echo "$root/target/pgo/use/release/weavepy"
