# Small-slot census

This archive records the completed small-slot vector candidate before the
JIT constant-load changes. The executable is preserved locally as
`target/release/weavepy-runtime-small-slots`; its checksum is recorded in
`environment.json`. All 190 compatibility checks and the enumeration,
builder, and temporary-list lifetime checks pass.

`source-changes.diff` records tracked changes from the checkpoint. The new
untracked tuple-storage module is retained under `runtime-sources/`, regression
sources under `test-sources/`, and exact measurement scripts under
`measurement-scripts/`. Every saved input matches the environment checksums.
The layout source and optimized LLVM constants retain the release size check.
The standard suite and three-fixture repeat pair the preceding tuple-cache
release with the candidate as well as the original checkpoint and CPython.

The focused slot comparisons retain the eighth-field lookup regression
alongside allocation time and memory improvements. Other retained tuple-hash
and allocation comparisons in the report come from earlier archives and are
labeled accordingly. CPython still wins many time and memory comparisons.
