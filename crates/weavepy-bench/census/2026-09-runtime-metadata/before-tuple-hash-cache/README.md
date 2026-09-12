# Eight-byte hash-cache checkpoint

The measured executable is `target/release/weavepy-runtime-hash-normalization`,
SHA-256 `cd9d7362f00fb642f99a3c980811b0f794cf420cf24de6be7c332ce633d5eb01`,
containing 44,138,384 bytes. This checkpoint normalizes Python hash results,
shrinks hash caches from 16 to 8 bytes, propagates construction errors, and
copies exact dictionaries without rehashing their keys. It includes all
preceding metadata, JSON, enumeration, list-builder, and small-tuple work.

All 181 compatibility checks, 278 VM tests, 38 JIT tests, Clippy, workspace
checks, and compilation without the JIT pass. The release layouts are
176 bytes for an instance header and 64 bytes for a frozen-set header.
The six isolated allocation comparisons use 1.4–3.6% less peak process
memory than the immediately preceding release. Five workloads take less
time; the eight-slot case takes 1.004 times the preceding workload time.

The full standard suite uses 0.855 times the checkpoint's JIT workload time
and 0.989 times its peak RSS as geometric means. Against CPython, those
ratios are 3.712 and 2.181. Six of 23 timed workloads beat CPython; none of
the 24 standard fixtures uses less peak RSS. The universal goal remains unmet.

The [report](report.md) retains CPU use, startup, threaded execution,
allocation measurements, and regressions. Separate CPython datetime
diagnostics retain the substantial timing variability instead of replacing
the original census. Native constructors and arithmetic were confirmed in
the diagnostic processes; this doesn't explain the earlier CPU-time changes.

`source-changes.diff` and `environment.json` describe this release before
the subsequent tuple-hash-cache changes. Restore the diff against the
recorded baseline and the files in `test-sources/` and `measurement-scripts/`
at their recorded repository paths. Their checksums match the environment.
This archive contains no performance measurements of tuple hash caching.
