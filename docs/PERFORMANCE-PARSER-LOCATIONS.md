# Indexed parser source locations

Generated modules no longer recount every preceding newline after every
statement. Simple statements skip unused location work; compound-statement
metadata and block diagnostics share a lazily built newline index. Backward
queries, needed when enclosing statements finish, retain their original
UTF-8 byte positions. The location work changes from repeated prefix scans
to one source scan and logarithmic indexed queries.

## Measurement

Measured on macOS x86-64 against `eea2745` (the runtime at `691af62`), with
CPython 3.14.5 built with PGO, LTO, and its tail-call interpreter. CPython's
experimental JIT is off. Runs alternate order, discard one warmup cycle,
and verify separate populated frozen caches stay unchanged. No build, test,
profile, or other benchmark overlaps measured runs. Lower ratios are better.

Both probes include source construction, compilation, execution, and result
checks in their timers. `bench_uncacheable_calls.py` defines and retains
distinct wrappers and calls each four times; `bench_compile_statements.py`
compiles and executes repeated additions. Seven paired cycles per size:

| Probe | Size | JIT/base time | Interp/base time | JIT/CP time | JIT/base CPU | JIT/base RSS | JIT/CP RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| functions | 2000 | 0.502 | 0.458 | 1.152 | 0.655 | 1.000 | 1.230 |
| functions | 10000 | 0.193 | 0.191 | 1.198 | 0.212 | 1.000 | 1.193 |
| functions | 20000 | 0.108 | 0.106 | 1.169 | 0.116 | 1.002 | 1.209 |
| statements | 2000 | 0.506 | 0.491 | 0.668 | 0.863 | 0.999 | 1.086 |
| statements | 10000 | 0.222 | 0.228 | 0.853 | 0.359 | 1.002 | 1.082 |
| statements | 20000 | 0.120 | 0.116 | 0.844 | 0.165 | 0.923 | 1.034 |

The 20,000-function case improves about 9.3 times; it still takes 17% more
workload time than CPython. Simple-statement cases beat CPython's workload
time but retain higher peak RSS. These are specific compilation improvements,
not proof of universal CPython parity.

The unchanged application suite uses three paired cycles of the existing 24
fixtures and work sizes. Workload time excludes the startup fixture; elapsed,
CPU, and RSS include all 24. Geometric means:

| Metric | JIT/base | Interp/base | JIT/CP | Interp/CP |
| --- | ---: | ---: | ---: | ---: |
| Workload time | 1.000 | 1.011 | 1.086 | 2.211 |
| Elapsed | 0.993 | 0.998 | 1.354 | 1.927 |
| CPU | 0.993 | 1.000 | 1.292 | 1.894 |
| Peak RSS | 1.000 | 0.988 | 1.575 | 1.342 |

## Costs and repeated checks

Seven-cycle rechecks retain mixed results. The application recheck ratios
(JIT/interpreted workload time) are:

| Fixture | JIT/base | Interp/base |
| --- | ---: | ---: |
| pyaes | 0.972 | 1.006 |
| richards | 0.990 | 1.010 |
| spectral_norm | 0.956 | 1.053 |
| dict_ops | 1.038 | 1.026 |
| list_ops | 0.989 | 1.077 |
| call_overhead | 1.035 | 1.065 |
| generators | 1.004 | 1.024 |
| deque_ops | 1.002 | 1.063 |

Longer seven-cycle checks, including cases with confirmed costs:

| Fixture | Work | JIT/base | Interp/base | JIT/base RSS |
| --- | ---: | ---: | ---: | ---: |
| sumvm | 20,000,000 | 0.983 | 1.089 | 0.997 |
| jitloop | 3,000 | 0.993 | 1.066 | 0.991 |
| list_ops | 100,000 | 1.019 | 1.035 | 0.960 |
| jitkernels | 20,000 | 1.031 | 1.017 | 1.003 |
| generators | 500,000 | 1.068 | 0.978 | 1.011 |
| call_overhead | 1,500,000 | 1.004 | 0.993 | 0.995 |

The long interpreted `sumvm` and `jitloop` costs persist at about 9% and 7%.
The long JIT generator case costs about 7%; the long call result is near the
baseline. These are measured costs, even though this patch changes parser
code rather than interpreter execution. Their cause hasn't been isolated.

Two independent 31-cycle startup sweeps use unchanged populated caches.
The table shows candidate/base elapsed and CPU ratios in each sweep:

| Mode | JIT elapsed 1 / 2 | JIT CPU 1 / 2 | Interp elapsed 1 / 2 | Interp CPU 1 / 2 |
| --- | ---: | ---: | ---: | ---: |
| pass | 1.007 / 1.030 | 0.999 / 1.051 | 0.988 / 0.974 | 1.010 / 0.973 |
| no_site | 1.022 / 0.964 | 1.061 / 0.997 | 0.968 / 0.912 | 0.969 / 0.977 |
| isolated | 1.006 / 1.004 | 0.999 / 1.029 | 0.981 / 0.967 | 0.987 / 0.994 |
| imports | 1.004 / 1.006 | 1.005 / 0.996 | 0.997 / 0.986 | 0.990 / 0.984 |

Default startup costs up to 3% elapsed and 5% CPU in the second sweep.
The no-site CPU cost from the first sweep doesn't repeat. Default startup
still costs about 1.53 times CPython's elapsed time and 1.23 times its RSS
in the second sweep; imports cost 2.52 times its elapsed time and 2.09 times
its RSS. No-site startup remains ahead on elapsed time, CPU, and RSS.

The seven-cycle fresh-worker probe at work 100 is 0.993 times baseline JIT
time, 1.028 times interpreted time, and 1.009 times JIT RSS. Its JIT time
is still 4.405 times CPython's, with 1.823 times its RSS.

This change retains the large asymptotic compilation improvement while
recording the runtime costs for further work. It doesn't establish an
improvement on every workload or memory metric.

## Validation and limits

All 24 parser tests, 38 compiler tests, and 376 VM tests pass. Strict parser
Clippy passes without exemptions. Four new Rust tests cover lazy allocation,
backward nested completion, missing-block diagnostics, and equivalence with
the previous scan at Unicode, LF, CRLF, bare-CR, and source-boundary positions.
The Python fixture passes CPython and the baseline in all three WeavePy modes.
The candidate passes 76 release scripts in JIT, interpreter-only, and
GIL-disabled modes (228 runs), twelve scalar checks, and six compilation-probe
checks. Compiling all 246 repository Python fixtures produces byte-identical
serialized code, including source positions and exception tables.

The first diagnostic draft exposed existing CRLF syntax-error line and Unicode
last-statement metadata differences from CPython. They are recorded with the
original failing checks in research storage and aren't claimed fixed here.
This change doesn't establish a Windows cold-JIT fix; that benchmark gate
remains unresolved. Gates, fixture work sizes, and CI baselines are unchanged.

The frozen candidate SHA-256 is
`895a45f943d44164806f9e74c306cfbb889b1c56692af62532875d168998ac37`;
the baseline is
`4b926898036683065932b2c12bb2f1f580a6e855e3bc7e1c01506c79249d59f6`.
Both were built with `cargo build --release -p weavepy-cli --bin weavepy`.
The candidate is 51,819,040 bytes, 312 bytes larger. It was copied only after
the build exited successfully. Raw samples are under
`target/performance/parser-*.json`; profiles, source hashes, corpus comparisons,
and validation logs are under `target/performance/many-function-investigation/`.
