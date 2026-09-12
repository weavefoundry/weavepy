# Private GC-index hash mixing

The collector's private address-to-handle index mixes high hash bits into low
bits to distribute aligned object addresses across more home buckets. Global
Fx hashing, Python dictionaries and sets, object identity, ownership, and locks
retain their behavior. No unsafe code or public API change is introduced.

The [report](REPORT.md) covers the full 24-fixture census, 229 compatibility
checks, 303 VM and 52 JIT tests, retained-container controls, explicit collection
pauses, cold and warm standard workloads, allocation/numeric measurements,
startup/imports, and parallel execution. Frozen inputs, release/library hashes,
all raw process samples, and all regressions remain available.

The isolated index experiment retains 15 paired cycles for each of 12 actual
100,000-address inputs. It shows faster insertion and lookup but often slower
reverse deletion. Those are index-only measurements; real VM results determine
the combined effect. Matching release-library object layouts remain unchanged.
The CPython-wide performance objective remains unachieved.
