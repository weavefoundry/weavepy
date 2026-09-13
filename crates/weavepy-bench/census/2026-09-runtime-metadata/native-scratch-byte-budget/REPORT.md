# Active runtime checkpoint: bounded native scratch retention

The September 13 PR checkpoint builds the phase 79 runtime. All 340 source
hashes match its completed validation: 353 VM tests, 156 C API tests, strict
lint and format checks, no-JIT checks, 99 targeted checks, 44 independent
CPython result checks, and 275 compatibility checks passed.

Phase 78 consolidated nested native-call scratch storage. This revision adds
a 16 KiB retained element-capacity budget per scratch element type, alongside
the existing 64-entry cap. Oversized active calls remain supported. The two
pools retain at most 32 KiB of element capacity per thread; this does not bound
active storage, allocator overhead, or process RSS.

Both timing gates stopped before starting a benchmark because host load was
too high. This revision is validated but unmeasured. The phase 78 predecessor
measured about 1.5% less full-suite time against the retained phase 69 reference;
that result is not a measurement of this additional capacity limit.

The CLI built with `cargo build --release -p weavepy-cli` was 44,097,264 bytes,
SHA-256 `4c627e81ee77bd25ec6140fdfd13f13c0886c82d0dddca0be1d476661c2f0c76`.
The archive contains sources, methods, inputs, validation, both unsuccessful
gate attempts, and their original limitations. Build products and caches are
excluded. The separate phases 80 through 96 have not been overlaid onto this
runtime; their source archives preserve that experimental work.
