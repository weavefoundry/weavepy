# Direct tuple enumeration checkpoint

This stage enumerates exact tuples of immutable values without constructing a temporary pair object. Its executable is `target/release/weavepy-runtime-tuple-pairs`, SHA-256 `ce5ca836db0e644a148ae574991a10be64b6fc226b7e4af8ac957cb529c0abfb`, containing 44,100,416 bytes.

Nine paired cycles against the preceding compact-storage build reduce generic enumeration time to 0.219 and 0.217 times that build. The two probes remain 1.053 and 1.052 times CPython's workload time. Byte enumeration regresses by 8 percent in the same run, while remaining faster than CPython; that regression is retained in the raw data.

All 276 VM unit tests, 38 JIT tests, Clippy, workspace checks, and compilation without the JIT pass. The Python enumeration and builder tests pass in all runtime modes, and the checked compiled kernels have no repeated exits. All 172 compatibility checks pass, including the CPython threading, weak-reference, I/O, and pickle suites. No separate complete standard-suite census is claimed for this intermediate build.

The source and environment were captured before direct small-tuple allocation and inline scalar cloning were added.
