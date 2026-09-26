# Hash internal JIT code identities directly

The scalar-call profile after `c5b1ce5` spends 222 of 1,560 active worker samples in
`hash_one` and SipHash, about 14.2%. The other sampled thread is idle and excluded. JIT
compilation and return-lane caches use allocated code-object addresses as keys; they now
use the VM's existing Fx word fold with the same upper-to-lower bit mixing used by the
GC index. Mixing avoids clustering aligned addresses into a subset of home buckets.

Only these two private code-object maps change. Their key ownership, weak references,
eviction, compilation generations, and lookup behavior remain intact. String-keyed maps
and Python-visible hashing are unchanged. This adds no dependency and changes no JIT
admission, budgets, work sizes, or benchmark gates.

## Validation

All 385 VM tests, the unchanged 1 MiB embedding test, VM Clippy with its two existing
exclusions, scoped formatting, and no-default-feature compilation pass. JIT sources and
benchmark tools are unchanged from their preceding complete test/lint passes. The frozen
release passes 276 regression runs across 92 fixtures in JIT, interpreter-only, and GIL-
disabled modes, plus 256 probe checks. Code/cache collection, worker teardown,
function/default replacement, recursion, observers, namespace callbacks, shared storage,
writable caller locals, and native scratch lifetimes remain covered.

The release binary is 51,868,736 bytes, 4,192 bytes smaller than the preceding accepted
binary. Its SHA-256 is
`073b21341586b79c78db5f11c42b48fc4b334e0d53e607a7faeb827d20bb7512`; the baseline is
`553f095b76a8eae3f0074aab14aa209b09b762fca67f4a76f93ee036afffa187`. The release build
completed successfully, and a fresh process inventory confirmed the compiler and its
controller had exited before the executable was frozen. Sources, hashes, immutable
binaries, profiles, and raw measurements remain under `target/performance/jit-code-
identity-hashing-investigation/` and `target/performance/jit-code-identities-*`.

## Measurements

The baseline is `c5b1ce5`. Measurements use macOS x86-64 and optimized CPython 3.14.5,
with the unchanged paired methodology in [Borrowed scalar calls](PERFORMANCE-BORROWED-
SCALAR-CALLS.md). Setup and result checks stay timed. The standard scalar probes use
seven paired cycles and 200,000 calls; the sustained probes use five cycles and one
million calls. Ratios below are candidate/baseline; lower is better.

| Callback | Standard JIT work | Process elapsed | Process CPU | Peak RSS | Sustained JIT work |
| --- | ---: | ---: | ---: | ---: | ---: |
| One argument | 0.824 | 0.879 | 0.896 | 0.998 | 0.817 |
| Two arguments | 0.820 | 0.962 | 0.938 | 1.006 | 0.822 |
| Guarded global | 0.868 | 0.958 | 0.937 | 0.999 | 0.887 |
| Trailing default | 0.876 | 0.974 | 0.967 | 1.002 | 0.859 |
| Constant return | 0.996 | 1.000 | 1.009 | 1.004 | 1.006 |
| Branch | 0.987 | 0.997 | 1.009 | 1.008 | 0.945 |

Simple callback workload time is now 0.945/0.981 times CPython at standard work and
0.863/0.914 times at sustained work. This does not establish whole-process or memory
parity: sustained elapsed ratios against CPython are 1.061/1.118, CPU 0.976/1.058, and
RSS 1.455/1.457. Globals/defaults still take 1.111/1.149 times CPython's sustained
workload time. Sustained two-argument/global RSS increases 1.3%/1.7% against baseline.
Separate traces show identical compilation and native-call counts for every scalar
probe, including 199,950 scalar native calls and no native-call deopts in each
simple/global/default probe.

Three cycles of the unchanged 24-fixture suite give default-JIT geometric means of 1.012
workload time, 0.999 process elapsed, 1.002 CPU, and 1.004 peak RSS. Interpreter-only
means are 0.994, 0.993, 0.990, and 1.004. Workload means exclude startup. Default-JIT
means against CPython are 1.112, 1.427, 1.330, and 1.611, respectively. Overall CPython
parity remains unachieved.

The initial broad run records string/dictionary JIT work at 1.078/1.065. Seven-cycle
rechecks give 1.002/1.006. Initial Fannkuch interpreter work of 1.034 repeats at 0.998,
and generator JIT work of 1.038 repeats at 0.994. Smaller costs remain: repeated call-
overhead JIT work is 1.042, attribute JIT work 1.017, pidigits interpreter work 1.037,
DeltaBlue interpreter work 1.024, and deque interpreter work 1.026. At separately
increased work of 200,000, five-cycle call/attribute JIT ratios are 1.020/1.005;
interpreter ratios are 0.998/1.017. These larger controls do not replace the standard
measurements.

List JIT RSS is initially 1.024 and rises to 1.111 in the seven-cycle repeat. A 31-cycle
recheck at the original work gives 0.995 RSS and 0.993 JIT workload time. A separate
31-cycle identical-baseline-binary control gives 1.011 RSS and 0.999 work, showing
substantial memory variation between runs. All original data remain recorded; none of
the rechecks replace the original suite means.

Thirty-one startup cycles initially give default-JIT elapsed ratios of 1.024 ordinary,
1.018 no-site, and 1.036 isolated. A separate complete 31-cycle repeat gives 0.997
ordinary, 1.016 no-site, 1.028 isolated, and 1.009 imports. Repeated no-site/isolated
CPU remains 1.025/1.052, and ordinary RSS remains 1.011. These startup costs remain
open.
