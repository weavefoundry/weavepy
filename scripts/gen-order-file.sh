#!/bin/sh
# Regenerate crates/weavepy-cli/weavepy.order: the binary's functions in
# the order interpreter start-up and a few short workloads first run
# them, as `llvm-profdata order` lays them out to minimise page faults.
# crates/weavepy-cli/build.rs hands the file to the macOS linker
# (`-order_file`), so start-up touches a few contiguous megabytes of
# code instead of pages scattered across the whole text segment.
#
# Needs the `llvm-tools` rustup component and python3. Takes one
# instrumented release build (in target/order/, kept apart from
# target/release/) plus the regular release binary. The instrumented build
# skips LTO, which drops the temporal-profile flag, and names the host
# target explicitly so build scripts stay uninstrumented; both change
# cargo's crate hashes, so the names are then remapped onto the
# release binary's (scripts/remap-order-file.py).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/target/order"
RAW="$OUT/raw"
HOST=$(rustc -vV | sed -n 's/^host: //p')
PROFDATA="$(rustc --print sysroot)/lib/rustlib/$HOST/bin/llvm-profdata"
rm -rf "$RAW"
mkdir -p "$RAW"
REL="$ROOT/target/release/weavepy"
[ -x "$REL" ] || (cd "$ROOT" && cargo build --release -p weavepy-cli)
CARGO_TARGET_DIR="$OUT" \
WEAVEPY_NO_ORDER_FILE=1 \
CARGO_PROFILE_RELEASE_LTO=off \
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
RUSTFLAGS="-Cprofile-generate=$RAW -Cllvm-args=-pgo-temporal-instrumentation" \
    cargo build --release -p weavepy-cli --target "$HOST"
BIN="$OUT/$HOST/release/weavepy"
# One raw profile per process: temporal profiles cannot be merged
# online, so no `%m` in the pattern.
run() { LLVM_PROFILE_FILE="$RAW/run-%p.profraw" "$BIN" "$@" >/dev/null 2>&1 || true; }
# Start-up in its common shapes, then short runs of the benchmark
# fixtures (their first-use order after start-up).
for _ in 1 2 3; do
    run -c pass
    run -S -c pass
    run -I -c pass
done
for f in "$ROOT"/crates/weavepy-bench/fixtures/*.py; do
    WEAVEPY_BENCH_WORK=1 run "$f"
done
"$PROFDATA" merge -o "$OUT/merged.profdata" "$RAW"/run-*.profraw
"$PROFDATA" order "$OUT/merged.profdata" --output="$OUT/order.txt"
# Mach-O symbol names carry a leading underscore; drop the comments.
sed -e '/^#/d' -e 's/^/_/' "$OUT/order.txt" > "$OUT/remapped.order"
python3 "$ROOT/scripts/remap-order-file.py" "$OUT/remapped.order" "$BIN" "$REL"
cp "$OUT/remapped.order" "$ROOT/crates/weavepy-cli/weavepy.order"
