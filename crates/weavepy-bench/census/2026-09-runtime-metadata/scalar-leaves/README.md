# Bounded scalar native calls

The frozen release is `weavepy-runtime-scalar-leaves`, SHA-256
`3b4bbff67dc2394129d14f4780f5ba5402612f03b25bb5cecc30c880fa47779c`,
44,157,712 bytes. It adds a bounded, helper-free scalar call path and rejects
missing constant and scalar helper registrations at standalone JIT compilation.

All 202 compatibility checks passed, as did 49 JIT tests, 283 VM tests,
Clippy, workspace/all-feature checks, and the CLI without its JIT feature.
Focused debug and release proofs include 105,398 native scalar-leaf entries,
long completed-call loops, constants, scalar arithmetic, enumeration, and list
builders/lifetimes. Exact sources and scripts are in `inputs/`.

The [complete focused tables](MEASUREMENTS.md) show 51–53% lower workload
time for direct integer callbacks versus the helper-inlining release. The
broader call fixture improves about 6%, but remains about 10% slower than the
small-slot release. Keyword and callable-instance controls remain close to
their baseline. Peak RSS is approximately unchanged in these comparisons.

The cold nested-loop repeat regresses 16.6% in its workload timer, while process
CPU time is nearly unchanged. Separate [warmed repeats](NESTED-REPEAT.md) at
work sizes 120 and 240 have time ratios of 0.995 and 1.002 against the same
baseline. This points to entry or compilation costs rather than a persistent
loop slowdown; the precise cause is unresolved. Both sets of samples remain.

Later artifact-sharing changes are excluded from these frozen runtime sources
and validation results. There is no separate full standard-suite census for
this intermediate candidate.
