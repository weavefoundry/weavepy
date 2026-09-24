# Borrow dynamic attribute receivers and move completed results

The native dynamic-attribute helper previously cloned its already-pinned
receiver before every lookup, then cloned a completed result into another pin.
It now borrows an ordinary object pin throughout the callback-free lookup and
moves the completed result into its destination. A specialized list pin keeps
its temporary wrapper; a captured bound method still owns its receiver.

Lookup, callback-frame reconstruction, roundtrip accounting, and pin limits
are unchanged. `None` retains its nullable sentinel. A result that can't be
pinned is parked once for the interpreter. This change adds no cache metadata,
result lanes, or pin reuse. The JIT analyzer, IR, and helper ABI are unchanged.

## Measurements

Seven alternating paired cycles compare the preserved 430a07d runtime,
this candidate, and optimized CPython 3.14.5 on macOS x86-64. CPython's
experimental JIT is disabled. Ratios are medians of paired measurements;
lower is better. The attribute probe reads two names 100,000 times, with
receiver setup outside the timer. Warm cases first run an untimed invocation.

| Read | JIT work/baseline | JIT RSS/baseline | JIT work/CPython |
| --- | ---: | ---: | ---: |
| class-cold | 0.782 | 1.008 | 2.948 |
| class-warm | 0.749 | 1.001 | 2.600 |
| module-cold | 0.720 | 1.004 | 2.269 |
| module-warm | 0.704 | 0.992 | 1.884 |
| property-cold | 0.878 | 0.996 | 2.887 |
| property-warm | 0.822 | 0.991 | 2.662 |
| getattr-cold | 0.931 | 0.993 | 1.766 |
| getattr-warm | 0.900 | 1.001 | 1.645 |

The chain probe performs one million iterations with construction inside its
timer. It uses seven paired cycles per shape.

| Chain | JIT work/baseline | Interpreter work/baseline | JIT RSS/baseline | JIT work/CPython |
| --- | ---: | ---: | ---: | ---: |
| dict-4 | 1.054 | 0.997 | 1.009 | 0.727 |
| dict-8 | 0.902 | 1.024 | 1.001 | 0.808 |
| dict-9 | 0.840 | 1.016 | 0.973 | 1.364 |
| dict-16 | 0.644 | 0.990 | 0.991 | 5.147 |
| slots-16 | 0.674 | 0.978 | 0.979 | 6.704 |
| mixed-16 | 0.651 | 0.969 | 0.957 | 5.589 |

An independent seven-cycle screen takes 1.064 times baseline on four-field
chains and 0.996 times on eight-field chains. The four-field cost remains in
both samples; the eight-field gain is variable. Both short chains still beat
CPython in workload time. Dynamic attribute and long-chain timings remain
slower than CPython, and their process memory remains higher.

The unchanged 24-fixture suite ran three paired cycles. Workload means exclude
startup; process means include it. These averages don't establish an overall
application speed improvement.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.006 | 1.002 | 1.090 |
| Process elapsed | 1.008 | 1.003 | 1.356 |
| Process CPU | 1.011 | 0.999 | 1.311 |
| Peak RSS | 1.002 | 1.004 | 1.583 |

Seven-cycle rechecks cover every application row whose full pass or initial
screen had any increase above 3%. Sumvm, nested loops, jitloop, Richards,
dictionary operations, generators, and the earlier process-only PyAES/deque
increases don't persist. Interpreter-only N-body remains 6.5% slower, with
7.7% more process elapsed time and 7.5% more CPU. Floating-point JIT workload
time is 2.8% higher and CPU is 3.0% higher. These residual costs, the repeated
four-field slowdown, and the worker increase remain part of the result.

Thirty-one paired startup cycles give default-JIT elapsed ratios of 1.007 for
normal startup, 1.017 for isolated startup, 1.021 for `-S`, and 1.002 for
imports. Normal startup still takes 1.443 times CPython's elapsed time and
1.228 times its RSS; imports take 2.681 and 2.249 times, respectively. `-S`
remains faster and smaller than CPython, at 0.807 and 0.757 times.

Seven-cycle worker and replacement probes retain small costs. The 100-worker
probe takes 1.032 times baseline work and 1.019 times RSS. Four-field
replacement takes 0.997/1.014 times work/RSS; eight-field replacement takes
1.002/1.014 times. Worker time remains 4.675 times CPython, while replacements
take about twice its time and 1.52 to 1.56 times its RSS.

Builds, tests, profiles, and timed runs never overlap. Every comparison uses
the same staged standard library, unchanged work sizes, and isolated frozen
caches whose manifests remain stable during measurement. No CI gate or
benchmark baseline was changed.

An earlier integer-result fusion candidate avoided result pins and unboxing
calls, but repeated checks retained interpreter workload costs. Adding receiver
borrowing to it improved focused class/module reads another 23% to 31%, while
numeric costs persisted. Those candidates remain uncommitted and preserved for
comparison. This smaller change isolates reference ownership. The interpreter
dispatch function returns to the accepted binary's address; that observation
doesn't establish a cause for timing differences.

## Validation and provenance

All 372 VM tests and 171 release regression runs pass. The release checks cover
JIT, interpreter-only, and free-threaded modes. Strict JIT Clippy and VM Clippy
with the existing local exclusions pass. The JIT crate's production source
is unchanged.
The new isolated ownership test sizes its own 8 MiB worker, verifies nine
compiled drivers, and checks more than 1,000 native reads in each of the class,
module, builtin, and getter paths. Its Python fixture also passes on CPython
and the accepted runtime in all three modes.

The fixture covers self-returning and heap-valued getters, nullable results,
class/module value replacement, and bound methods outliving their original
receiver variable. Existing fixtures retain coverage of callbacks, code
replacement, caller-frame visibility, tracing, and collection. Intermediate
attribute pins still remain; this doesn't resolve arbitrary native pin lifetime.

The candidate, `weavepy-borrowed-only-attributes`, has SHA-256
`5a165c3dba2e5cde5ab13023804cff4d1d9acba9dcdb2d5192b4d0f536cd57cb`.
It is 51,795,256 bytes, the same size as baseline. Its source patch from
430a07d has SHA-256
`20f61f2daaaedb7c3b1c0e046168be0ec511380f42a6e34fc5e8b224ac03f15a`.
Raw samples, fixture hashes, source patches, and build/test logs remain under
`target/performance/borrowed-only-*` and `borrowed-only-experiment/`.

Reproduce the focused comparison with `tools/bench_compare.py --probe
tools/bench_dynamic_attributes.py --work 100000 --samples 7`, selecting
`WEAVEPY_DYNAMIC_ATTRIBUTE_KIND=class`, `module`, `property`, or `getattr`.
Use `--warm` for warmed cases, the preserved baseline/candidate binaries, and
a fresh `--frozen-cache-root` under `target/`. Build the CLI with
`cargo build --release -p weavepy-cli --bin weavepy`.
