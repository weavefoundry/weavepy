# Native string callback repair

This correctness-only release rejects callback-capable `str.replace` count
objects before invoking the builtin body. The interpreter then executes the
call once with the correct frame and observes callback mutations. Exact
primitive counts remain native. No new unsafe code, public Rust API, or helper
ABI is introduced.

Candidate SHA-256: `d804605147887eb045126571a553fc9af27d99f5de1a2abbd1bbc2375dfb4ae6`;
44,194,256 bytes. All 12 measured release-library layouts are unchanged.
All 308 VM tests, 52 JIT tests, Clippy, and workspace/all-feature/no-JIT checks
pass. The focused callback test failed before the guard and passes afterward.
Release probes confirm global updates, caller frames, callback counts, raised
exceptions, and the existing completed-string/list expression-stack behavior
in JIT, interpreted, and GIL-disabled modes, matching CPython. Traces show
`replace_once` reconstructing at its CALL. The preceding release's wrong global
and caller-frame results are preserved alongside the corrected results.

The full 238-check compatibility script and paired performance script were
prepared but were NOT run for this release. No speed or memory improvement is
claimed for this correctness-only stage. The frozen snapshot contains 113
sources and 37 measurement inputs.

A further probe, run after the callback repair, finds that split-result lists
are not registered with the collector: `gc.is_tracked` is false, `gc.get_objects`
does not include them, and a marker in a list self-cycle survives `gc.collect`.
CPython returns true for tracking and visibility and reclaims the marker.
Both GC-index and this release fail in JIT mode, and this release fails in
interpreter mode. This defect is not repaired here. It also means earlier
zero-result sentinel-list probes could not rule out retention of untracked
split results. Their negative observations must not be treated as proof that
no such retention exists. The split-list repair takes priority before the next
performance batch. Missing replace-argument validation also remains unresolved.

The CPython-wide performance objective remains unachieved. The GC-index archive
is still the latest complete 24-fixture census. No controlled build-latency,
energy, or free-threaded CPython claim is made.
