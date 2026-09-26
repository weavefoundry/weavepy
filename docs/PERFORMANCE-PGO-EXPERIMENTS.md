# Profile-guided release experiments

Profile-guided optimization (PGO) remains experimental. It isn't enabled in the
release build or CI. Two profiles built from commit `4377ce1` improved aggregate
execution time and memory use, but repeated individual regressions prevented
adoption. No benchmark, baseline, or acceptance threshold changed.

## Method

These measurements use native x86-64 macOS on an Intel Core i9-9980HK, Rust
1.94.0 with LLVM 21.1.8, and CPython 3.14.5 with PGO, LTO, and its tail-call
interpreter. CPython's JIT was off. An explicit-target, uninstrumented release
provides the same-source build reference; the ordinary release at `4377ce1`
provides a second control. Profiles use the matching Rust LLVM tools and retain
zero counts. Each executable was frozen and hashed after its build finished.
Builds, validation, and profiling did not overlap timed comparisons.

The first corpus exercises numeric operations, containers, objects, text,
serialization, AST processing, iterators, and dates at three sizes in JIT,
interpreter, and GIL-disabled modes. All 72 instrumented executions matched
CPython. The expanded corpus adds independent arbitrary-precision arithmetic,
scalar loops, dynamic dispatch, and double-ended queues: another 12 checked
executions. It uses programs from earlier, unadopted experiments at `547cfc8`;
it isn't a never-tuned holdout. Training doesn't import the timed fixtures.

Each normal and optimized executable passed 294 regression runs covering 98
fixtures and 848 semantic probes. All 43 inspected compilation decisions and
JIT statistics matched. Both profile-use builds reported 1,093 functions without
profile data, with no control-flow mismatch warnings or discarded nonzero
counts. This doesn't establish complete profile coverage.

## Results and decision

Ratios below are optimized divided by the ordinary accepted release, so lower
is better. The unchanged 24-fixture sweep uses three interleaved cycles.
Workload averages exclude the startup fixture; process measurements include it.

| Default JIT geometric mean | Initial profile | Expanded profile |
| --- | ---: | ---: |
| Workload time | 0.939 | 0.932 |
| Process elapsed time | 0.955 | 0.934 |
| Process CPU time | 0.948 | 0.926 |
| Peak resident memory | 0.899 | 0.893 |

The initial profile makes big-integer execution roughly 30% slower. Expanded
training fixes the cold `pidigits` result, but its seven-cycle warm recheck is
still 1.239 times the accepted release. Expanded-profile AES is 1.052 times
accepted cold and 1.087 times accepted warm. At 20 million iterations, the
interpreter's `sumvm` ratio is 1.081. These costs remain in the decision even
though the aggregate improves. Neither profile is adopted. Focused application,
fallback-family, and extended AST comparisons were prepared but weren't run
after these repeated costs established the hold.

Against CPython, the expanded profile's default-JIT geometric means remain
1.002 for workload time, 1.320 for elapsed time, 1.199 for CPU time, and 1.383
for peak resident memory. These are experimental measurements, not results for
the shipped binary, and they don't establish superiority across workloads.

The normal executable is 51,877,040 bytes; the initial and expanded optimized
executables are 48,147,084 and 48,058,836 bytes. Observed build durations were
530 seconds for normal, 565 for instrumentation, and 595/581 for profile use.
These durations aren't a controlled build-speed comparison. A production
pipeline would also need to train, package, and validate the platform's shared
Python runtime, especially `python314.dll` on Windows; optimizing only the
Unix CLI doesn't cover that distribution.

Generated corpora, build commands, compiler diagnostics, executable hashes,
profiles, validation records, and every original timing sample are retained in
`target/performance/profile-guided-release-investigation/`. Earlier PGO trials
remain in `target/performance/pgo-experiment/`; neither directory is tracked.
Future experiments must use new output and cache names and retain their
predecessors. Don't overwrite a held result with a favorable recheck.
