# Initial small-tuple measurements

These focused measurements use `weavepy-runtime-small-tuples` before its full 177-check compatibility run and subsequent complete census. VM unit tests, JIT tests, Clippy, workspace compilation, tuple lifetime checks in three runtime modes, and the CPython tuple and generator-expression suites had already passed.

The initial partition elapsed-time ratio was 1.152, despite a workload CPU ratio of 0.984. A separate 19-cycle repeat yielded 0.984 elapsed time, 0.983 workload CPU, and 0.999 peak RSS relative to the preceding tuple-enumeration build. Both runs remain here; the initial scheduling-sensitive samples were not replaced in this archive.
