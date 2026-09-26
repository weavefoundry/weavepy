# Restore hot-code admission during startup

The blanket startup compilation deferral introduced in this PR reduced process
startup costs but moved compiler initialization into the first user workload.
At `15d6ba2`, the Windows benchmark gate still retains a sumvm regression of
21.9% against the merge base after retry. Its separate cold/warm diagnostic
ratios are 1.205/1.004. This change backs out that deferral: naturally hot startup
code can compile again. It keeps single `site` initialization, nested startup
scope restoration, and the later import compilation budget. It doesn't
precompile an artificial workload or change benchmark admission rules.

The local reference is `15d6ba2`, not the PR merge base. Measurements use Intel
macOS, Rust 1.94.0, and CPython 3.14.5. Engine order alternates, warmup is discarded,
and nonempty warmed caches remain unchanged during measurement. Builds, tests,
and profiles finish first. The host is an active desktop; raw samples and the
process snapshot are retained. Ratios below are candidate/reference medians of
paired cycles; lower is better.

Fifteen cycles of the original numeric fixtures give these results. Warm work
is the second invocation in one process; both invocations remain included in
its process elapsed time.

| Fixture | Cold work | Warm work | Cold process elapsed | Warm process elapsed |
| --- | ---: | ---: | ---: | ---: |
| sumvm | 0.856 | 0.985 | 1.031 | 1.012 |
| nested_loops | 0.908 | 0.998 | 1.051 | 1.044 |
| jitloop | 0.932 | 1.001 | 1.018 | 1.041 |

Separate instrumented traces show four naturally compiled startup helpers in
the candidate and none in the reference. Sumvm's first compilation takes
0.331/0.883 ms in that diagnostic; neither binary compiles during the second
invocation. This demonstrates the change in admission timing, not a faster
compiler or a net process-time improvement.

Two independent 31-cycle startup sweeps retain the costs. Ordinary JIT startup's
elapsed/CPU/RSS ratios are 1.129/1.195/1.198 initially and
1.123/1.175/1.201 on repeat. Repeated isolated startup is
1.129/1.173/1.196, and imports are 1.058/1.069/1.074. No-site startup also costs
more despite not entering the changed scope: its initial elapsed/CPU ratios
are 1.046/1.048 and its repeat ratios are 1.033/1.101. These observations aren't
discarded or attributed to a proven cause. Repeated ordinary startup remains
1.840 times CPython's elapsed time and 1.461 times its peak RSS.

The unchanged 24-application suite uses three cycles. Work includes 23 fixtures;
process metrics also include startup. JIT work/process elapsed/CPU/RSS geometric
means are 0.970/1.034/1.045/1.055 against the reference and
0.987/1.434/1.295/1.625 against CPython. Interpreter/reference means are
1.007/1.009/1.005/0.994. This is a cold-work tradeoff, not an overall speedup.

Seven selected applications have eleven repeat cycles. Sumvm, nested_loops,
and jitloop work ratios are 0.853/0.886/0.948. DeltaBlue remains approximately
flat (0.994 JIT and 0.993 interpreter), but this doesn't resolve the separate
ARM macOS gate result: `15d6ba2` retains DeltaBlue +17.1% against the merge base
after retry. Dictionary JIT work is 0.998, with peak RSS 1.154. The selected
JIT work/process elapsed/CPU/RSS means are 0.948/1.033/1.045/1.042; they aren't
repeated full-suite means.

PyAES's initial interpreter work ratio of 1.108 persists at 1.097 in an
independent eleven-cycle check, with process elapsed/CPU 1.073/1.077. Its JIT
work ratio is 0.950. The interpreter cost remains unexplained and retained.

Seven-cycle weakref, callback, and watched-cycle controls retain JIT work ratios
of 0.999/1.005/0.990 against the preceding fixed-storage increment. Four import
probes give JIT work ratios between 0.982 and 1.007; first-use import RSS is
1.075. The startup rollback doesn't establish improvements to those workloads.

Validation passes 414 VM tests, nine comparison-harness tests, 34 C API unit
tests, direct weakref C API integration, the explicit 1 MiB embedding lifecycle,
Clippy with the existing local lint exceptions, and no-default compilation.
The frozen CLI passes 112 regression runs, ten additional GIL-disabled owner
and clearing checks, 476 probe shapes, eight observer runs, and twelve native
SQLAlchemy/Alembic checks. Startup tests cover hot leaf/loop admission, nested
scopes, unwinding, and reuse of the same compiled code. The earlier
[weakref compatibility qualifications](PERFORMANCE-FIXED-WEAKREF-STORAGE.md)
remain; this isn't a fresh complete upstream-suite validation.

The release CLI is 51,895,008 bytes, built with
`cargo build --release -p weavepy-cli --bin weavepy`. Its SHA-256 is
`058864ec140d2de58f1f9209b9a8001a19abe94be25d088a019d7fc5bd9a6dbd`;
the two-file runtime patch SHA-256 against `15d6ba2` is
`e94c5c2087b53412f4c8b8d5f2a1442ccb2cc519bc393b30f154f4fe2282638d`.
Source/fixture hashes, controllers, logs, all samples, and separate traces remain
under `target/performance/startup-admission-rollback-investigation/`. Binary,
source, sample-count, and cache checks pass after controller closure. The CI
gate's workloads, work sizes, retry policy, thresholds, and baselines are
unchanged. Cross-platform CI on the resulting commit is required before merge.
