# Conditional field getter experiments

Four conditional-getter candidates remain unmerged. They recognize a small function
that compares a receiver field with a global class constant and returns one of two
fields. The last candidate accelerates string selectors, but it doesn't establish
an application-level improvement and retains costs in unrelated controls. The
accepted runtime remains `5066f29`, with later embedding fixes and test diagnostics.

All candidates retain global/class versions, field-name checks, descriptor and
metaclass fallbacks, observer fences, comparison semantics, and the existing JIT
admission and compilation budgets. They add no persistent metadata or object owners.
The reusable fixture covers both branches, missing unselected fields, six comparison
operators, numeric boundaries, rich comparison and truth callbacks, caller-frame
visibility, descriptor and namespace mutation, returned-object lifetimes, tracing, and
repeated integer/string transitions. The probe times setup and result checks.

## Why the candidates are held

A scalar-stamp-only implementation regresses unsupported string selectors about
14-16%. An early tag check fixes initially unsupported strings but still regresses
integer-to-string transitions about 13-15%: the scalar stamp can remain stale after
class mutation. Borrowing the existing class-value cache fixes that fallback, but
its unoutlined implementation retains 2-6% costs in arithmetic and chained reads.
Its broad/startup batch is excluded from admission because formatting overlapped
part of the measurement interval; the isolated focused and sustained results are
sufficient to hold it.

The fourth candidate moves the conditional logic to a separate, non-inlined helper.
It borrows either the scalar stamp or an existing exact class value, reads only the
selected field, and falls back without callbacks or owner release. Its release
build and controller finished before freezing; all timing batches completed without
local builds, tests, profiles, or formatting. Measurements use macOS x86-64,
optimized CPython 3.14.5, and accepted `5066f29` as the paired baseline. Ratios below
are candidate/baseline; lower is better.

| Workload | JIT work | Interpreter work | Cycles |
| --- | ---: | ---: | ---: |
| String selector, left, one million operations | 0.763 | 0.764 | 5 |
| String selector, right, one million operations | 0.777 | 0.758 | 5 |
| Integer-to-string transition, left | 0.777 | 0.758 | 7 |
| Integer-to-string transition, right | 0.767 | 0.765 | 7 |
| Scalar return, left, one million operations | 0.973 | 0.954 | 5 |
| Scalar return, right, one million operations | 0.987 | 0.959 | 5 |
| Object return, left, one million operations | 0.991 | 0.964 | 5 |
| Object return, right, one million operations | 0.995 | 0.995 | 5 |
| DeltaBlue, focused | 1.006 | 1.005 | 7 |
| DeltaBlue, recheck | 1.012 | 0.992 | 7 |
| Richards, recheck | 1.019 | 0.991 | 7 |
| Dictionary operations, recheck | 1.037 | 1.030 | 7 |
| AES, broad | 1.016 | 1.027 | 3 |
| AES, recheck | 1.010 | 1.030 | 7 |

Sustained dictionary chains initially cost 3.2% JIT and 2.9% interpreter; the
seven-cycle recheck is 0.983/1.022. Dictionary arithmetic changes from 0.999/1.024
to 1.010/1.010; slot chains from 0.989/1.022 to 1.006/0.998. These original and
recheck measurements are retained together. Richards peak RSS is 1.014 in the
focused run and 1.013 on recheck, despite 0.960 in the smaller broad run.

Across the unchanged 24-fixture suite, the JIT geometric means are 0.999 work,
0.995 process elapsed, 0.998 CPU, and 0.998 peak RSS. The work mean excludes the
startup row. Interpreter means are 0.986, 0.982, 0.979, and 0.993. The JIT remains
behind CPython at 1.070 work, 1.380 elapsed, 1.280 CPU, and 1.598 RSS. These are
experimental results, not new accepted baselines.

All four startup cases use 31 cycles in both batches. Recheck JIT elapsed ratios
are 0.991 ordinary, 0.989 without site, 0.989 isolated, and 0.994 imports; RSS ratios
are 1.002, 1.005, 1.003, and 1.000. The first no-site CPU ratio of 1.049 doesn't
repeat: the second is 0.977. Startup and import gaps against CPython remain.

## Validation and provenance

The fourth candidate passes 388 VM tests, the embedding lifecycle, VM Clippy with
the two established exclusions, no-default compilation, scoped formatting,
14 benchmark-tool tests, 285 release runs across 95 regression fixtures, and
608 probe checks. Compilation decisions and native-call statistics match the
baseline in all 22 traced probes. These local checks don't resolve the separate
cross-platform pure-predicate coverage failure under investigation.

The frozen executable is 51,877,392 bytes, 4,288 bytes larger than the accepted
baseline and 4,200 bytes smaller than the unoutlined candidate. Its SHA-256 is
`296c9f1e833606e2945bbefbeb566cdafff3a3bcad489cc8ba130433dad68906`.
It was built from `3e1453d` plus source patch
`c38219cee1b06ae329cca4bae0bf1d718e2f4e33141339b3d3d91dbac1253b89`.
Later diagnostics change only test builds. Frozen executables, source snapshots,
controllers, validation logs, and raw measurements remain under
`target/performance/outlined-conditional-field-getters-investigation/` and
`target/performance/outlined-conditional-field-getters-*`. Earlier candidates
remain in the corresponding conditional, guarded-conditional, and
cached-conditional investigation directories.

The production shortcut is held. The subsequent [release-guard fix](PERFORMANCE-DEFERRED-INSTANCE-RELEASES.md)
resolves the predicate coverage failure on all three CI platforms. Further work
targets remaining release-guard costs and borrowed field arguments in complete
callers. Compiler-only paired loads already lower to supported instructions.
