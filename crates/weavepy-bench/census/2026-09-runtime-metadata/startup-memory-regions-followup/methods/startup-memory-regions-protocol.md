# Startup memory region diagnostic, not launched

After the current binding candidate's complete timing runs, inspect CPython,
retained539c, and the new release using the same small script. It imports os/time,
prints a PID and constant checksum, and stays alive for inspection. Use one
unmeasured seed process before each inspected process, per-binary frozen caches,
JIT enabled for WeavePy, and record binary/driver hashes, load, readiness values,
ps RSS snapshots, and complete vmmap -summary output. No other owned heavy work.

This is a point-in-time, instrumented diagnostic intended to distinguish mapped
code, allocator regions, and other contributors. It is not the original empty
startup benchmark, peak RSS, or a speed comparison. Preserve access failures and
all output; do not infer a complete map from a partial or failed inspection.
No malloc-history profile or allocation-count claim is included.
