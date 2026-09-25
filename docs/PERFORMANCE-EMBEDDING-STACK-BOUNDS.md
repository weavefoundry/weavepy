# Preserve stack bounds across embedding initialization

The Windows test for `5cc5fc9` overflows the unchanged 1 MiB embedding worker
during nested execution. Earlier runtime-identical commits alternated between
passing and failing this test.

The pinned stacker 0.1.24 Windows backend saves its cached stack limit inside
the new fiber. If that thread-local cache hasn't been initialized, its first
query discovers the temporary fiber's bounds. Returning to the caller restores
that temporary limit. Depending on address placement, a later `maybe_grow`
can overestimate the space remaining on the caller's small stack.

Embedding now queries the bounds on the caller's stack before its unconditional
stack switch. The existing 64 MiB growth policy and 1 MiB regression worker
remain unchanged. The regression checks the restored bounds after the first
`Py_Initialize`, without querying them beforehand and masking the defect.
Unknown bounds remain valid: stacker then grows conservatively. Nested
compilation, evaluation, cleanup, exceptions, and reinitialization retain
their existing assertions.

The embedding lifecycle passes locally on macOS x86-64. C-API formatting and
Clippy pass with the two previously documented VM lint exclusions. Windows
confirmation requires the new CI result; the local pass alone doesn't establish
it. This change makes no throughput or peak-memory claim. Source analysis,
the Windows failure log, and local validation remain under
`target/performance/embedding-stack-bounds-investigation/`.
