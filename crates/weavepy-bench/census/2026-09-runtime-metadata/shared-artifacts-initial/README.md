# Shared compiled artifacts

The frozen release is `weavepy-runtime-shared-artifacts`, SHA-256
`142ebdb2de69ef0de4aab94b80e81411e91c387ab536abdeef95de000e2d369c`,
44,158,432 bytes. It shares immutable compiled metadata through one reference
counted bundle. VM tests, Clippy, workspace and feature checks, and focused
debug/release proofs pass. There is no separate full compatibility or standard
performance census for this intermediate candidate.

The [complete focused tables](MEASUREMENTS.md) show another 9–11% reduction
in direct integer-call workload time versus the scalar-leaf release. Keyword
and callable-instance controls regress by about 4–5%, and the broad call
fixture is approximately unchanged. Every control and memory result remains
in the archive. Interpreter controls also vary, so these timings alone don't
isolate reference-count operations from other effects, such as code placement.

A later audit found a preexisting attribute-name hash collision affecting the
checkpoint and this candidate. The adjacent `exact-type-cache/` archive
contains its correction and the completed 205-check, 24-fixture census.
