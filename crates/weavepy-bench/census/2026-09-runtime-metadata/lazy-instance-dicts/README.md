# Lazy instance dictionaries

This archive contains the full 24-fixture census, all 223 compatibility checks,
41 focused controls, cold and warm regression controls, allocation/numeric
probes, startup/import probes, and parallel execution measurements. Its
[report](REPORT.md) identifies the binary and all comparison releases. Frozen
inputs contain 108 source files and 33 measurement inputs.

A pointer-sized LazyArc defers an instance dictionary's allocation until it is
needed. It preserves exported dictionary identity and shares later mutations
across shallow instance clones. Existing populated internal dictionaries retain
their Arc ownership. Seven ownership tests use the actual production module
under Rust 1.93 and Miri strict provenance with default and Tree Borrows on
ARM64 and i686. The production harness records its imported source hash.

Initial draft tests, the draft regression, and the draft probe script are
historical inputs, superseded by the sources in inputs/. The first standalone
compile failed for missing generic type annotations in two tests. The first
VM type check identified the Arc conversions needed by the new field type.
Clippy later rejected raw-string delimiter formatting in a new test. All were
corrected before the measured release. These failures remain in the archive.

The public Rust PyInstance.dict field type changes to sync::LazyArc. Python
exports continue to share the underlying dictionary, while Rust consumers
can convert an existing Arc with .into() or obtain one with .share(). This
change does not establish full free-threaded safety or enable native execution
without the GIL. All measured regressions remain visible. The CPython-wide
performance objective remains unachieved.
