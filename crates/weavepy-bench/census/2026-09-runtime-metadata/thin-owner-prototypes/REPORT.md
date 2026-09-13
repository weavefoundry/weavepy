# Isolated thin-owner prototypes

These are unapplied, standalone experiments. No WeavePy value layout has been
changed, and no speed, RSS, or Object-size improvement has been measured.
The overall performance goal remains unachieved.

The first prototype stores immutable u8/u32 slice lengths in their allocation
and retains Rust's Arc reference counting and wide Weak handles. Its two
ordinary tests and two strict-provenance Miri tests pass on aarch64-apple-darwin,
covering every length from 0 through 1024 and concurrent weak upgrades/drop.
The observed Miri test duration was 269.76 seconds, not a performance metric.

A current-source audit corrected the earlier design's scope: TupleStorage
has an unsized [Object] tail, so Object::Tuple is ALSO a wide pointer. Shrinking
Str, WStr, and Bytes alone cannot shrink Object. The retained design notes
explicitly record that correction. No layout claim should assume otherwise.

The second prototype generalizes the owner to a header and destructible tail.
Its three ordinary tests and all three Miri tests pass on both aarch64-apple-
darwin and i686-unknown-linux-gnu. They cover exact element/header destruction
at the last strong reference before final weak release, fixed-array unsizing,
unique mutation refusing existing strong or weak aliases, dynamic allocation
layout including empty values, and atomic-header thread handoffs. Observed Miri
test durations were 3.75 and 3.60 seconds, not runtime performance evidence.

Both prototypes retain std::sync::Arc's counts, synchronization, upgrades, and
destruction. A thin strong owner reconstructs metadata only while holding a
live strong reference. Wide weak handles retain their own metadata after the
payload dies. Dynamic construction uses equal-size/equal-alignment payload
conversion; it does not depend on private ArcInner fields. All safety comments,
exact sources, manifests, toolchain installation output, warnings, test logs,
and file hashes are retained. Zero-test doc-test output is not extra coverage.

Nightly 1.100.0 (0fc141305, 2026-09-11) was installed with Miri and matching
Rust sources without changing the default stable toolchain. Miri used
-Zmiri-strict-provenance. These finite checks aren't a soundness proof and do
not test a full VM, CachedHash, UTF-8 string API, C API mirrors, buffer exports,
frozen caches, or Python object populations. Production integration and broad
correctness/performance evaluation remain future work. Build caches, executables,
and installed toolchains remain local and aren't part of this archive.
