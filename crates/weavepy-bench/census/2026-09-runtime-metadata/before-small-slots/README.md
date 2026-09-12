# Tuple hash-cache census

This archive records the completed tuple hash-cache candidate before the
small-slot vector change. The executable is preserved locally as
`target/release/weavepy-runtime-tuple-hash-cache`; its checksum is recorded in
`environment.json`. All 187 compatibility checks pass, including the nested
C-API tuple suite's 14 cases. The initial zero-file discovery failure and
corrected explicit selection are retained in their own validation artifacts.

`source-changes.diff` records tracked changes from the checkpoint. The new
untracked tuple-storage module is retained under `runtime-sources/`, regression
sources under `test-sources/`, and exact measurement scripts under
`measurement-scripts/`. Every saved input matches the environment checksums.
The layout source and optimized LLVM constants retain the release size check.
The standalone allocation safety checks remain in [tuple-cache-miri](../tuple-cache-miri/).

The focused hash and allocation comparisons use the immediately preceding
hash-normalization release. They retain all workloads and regressions,
including increased tuple memory and the string-partition slowdown. The full
census still shows substantial time and memory gaps against CPython; this
archive doesn't establish universal superiority.
