# Native list cleanup checkpoint

This archive preserves the complete measurements and validation for `weavepy-runtime-list-reap`, before compact hash caches and inline instance slots. Its binary SHA-256 is `52caabfed72a5f5269e0e5eddcde0c4d325f26bc6c5219ba03ef3effcdc51aab`. The source diff and environment were captured before those later source edits.

All 169 compatibility checks pass. The standard workload geometric mean is 0.860 times the original checkpoint with the JIT enabled, but 3.343 times CPython. Peak RSS is 2.171 times CPython. See `report.md` and the raw paired samples for the complete results and limitations.
