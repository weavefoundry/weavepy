# Direct small-tuple allocation checkpoint

The measured executable is `target/release/weavepy-runtime-small-tuples`,
SHA-256 `43c70898ea0a57a7aa9ce2db7e6e2aeced2b5e3076c642f28dde525329920181`,
containing 44,119,520 bytes. Fixed-size tuple construction uses the final
allocation directly, and bytecode tuples of up to three elements retain the
existing free list. This checkpoint includes the preceding metadata, JSON,
enumeration, list-builder, cleanup, and inline-slot changes.

All 177 compatibility checks pass, along with 278 VM unit tests, 38 JIT tests,
Clippy, the workspace checks, and compilation without the JIT. The checked
enumeration and list-builder kernels compile without repeated exits.

The complete standard suite measures 0.857 times the checkpoint's JIT workload
time and 0.988 times its peak RSS as geometric means. Against CPython, those
ratios are 3.659 and 2.180. Six of 23 timed workloads beat CPython; none of the
24 standard fixtures uses less peak RSS. These results do not establish the
requested universal performance goal.

The initial full census has severe scheduling anomalies. One interpreted
attribute-access sample takes 23.634 seconds of workload time but only 1.328
seconds of total process CPU time. Baseline and CPython samples also show large
delays. The original samples remain in `suite.json`; `anomaly-repeat.json` and
`tuple-change-repeat.json` retain separate nine-cycle checks. Their findings
are recorded alongside the report when the repeats finish.

`source-changes.diff` and `environment.json` were captured before the hash
normalization change. `test-sources/` preserves the untracked tests named by
that environment, and `measurement-scripts/` preserves the scripts with
matching checksums. [The report](report.md) covers the complete census,
including CPU use, startup, parallel execution, allocation, and regressions.
To reproduce this checkpoint, apply the recorded diff to the baseline and
restore the saved tests and scripts at their recorded repository paths.
