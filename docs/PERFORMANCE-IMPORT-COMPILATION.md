# Import compilation performance

Fresh module execution now requires more evidence of sustained work before JIT
compilation. A thread-local phase distinguishes normal execution, imports, and
explicit startup deferral. Nested scopes restore their predecessor, including
on errors. Startup deferral takes precedence over nested imports. The ordinary
lean-call checkpoint is unchanged; during imports, repeated checkpoints count
toward 16 times their normal interval. Code can compile during a busy import or
at its next ordinary checkpoint after import. Loop and frame admission retains
the existing 16-times startup escape threshold.

This applies to the VM's three fresh-module execution paths. Cached imports,
public `exec`, and custom loaders that execute Python through another boundary
do not acquire a new import scope. No unsafe code or global threshold change is
introduced.

Measurements compare the candidate with runtime `472e26f` on macOS x86_64,
using Rust 1.94 and optimized CPython 3.14.5. Separate nonempty frozen caches are
warmed and checked for changes during timing. Variants are interleaved, warmup
is discarded, and builds, tests, profiles, and other benchmarks do not overlap.
Ratios are medians of matched cycles; lower is better.

## Import work

Seven-cycle probes include imports and first use inside the workload timer.
Module-loop and module-call probes also time source creation, temporary-file
handling, and cleanup. Their longer variants check whether later work loses the
initial import saving.

| Workload | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| First use, one iteration | 0.883 | 0.985 | 4.924 |
| First use, 100 iterations | 0.923 | 0.989 | 6.012 |
| First use, 1,000 iterations | 0.966 | 0.992 | 7.219 |
| Module loop, 1 million iterations | 0.937 | 0.998 | 1.253 |
| Module loop, 10 million iterations | 0.936 | 1.015 | 0.214 |
| Module calls, 100,000 iterations | 0.934 | 1.031 | 2.887 |
| Module calls, 3 million iterations | 1.010 | 0.932 | 0.830 |

The longer module-call probe has process elapsed/CPU ratios of 1.035/1.040,
despite its workload ratio of 1.010.

The initial first-use probe used a POSIX-only expected path. Before committing,
its assertion was made portable and the affected measurements were repeated.
The initial JIT/base ratios were 0.873, 0.883, and 0.963 at the three work sizes;
those samples remain in the research archive.

Three separate instrumented first-use runs fall from 45 compilation attempts
to one. Summed `compile_frame` time falls from 8.48-8.78 ms to about 0.006 ms.
That partial compiler timer is diagnostic evidence, not an end-to-end measure
or a replacement for the uninstrumented samples above.

Two independent 31-cycle startup sweeps give the following second-run ratios.
The first imports elapsed/CPU/RSS ratios were 0.925/0.927/0.930.

| Launch | JIT elapsed/base | JIT CPU/base | JIT RSS/base | JIT elapsed/CPython |
| --- | ---: | ---: | ---: | ---: |
| Normal | 0.993 | 1.002 | 1.003 | 1.415 |
| No site | 1.004 | 1.003 | 1.003 | 0.813 |
| Isolated | 0.999 | 1.007 | 1.003 | 1.438 |
| Imports | 0.935 | 0.924 | 0.928 | 2.405 |

## Application costs and remaining gaps

The unchanged full suite uses three cycles. Workload geometric means include
23 workloads; process means include all 24 fixtures, including startup.

| Metric | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.004 | 1.001 | 1.075 |
| Process elapsed | 0.999 | 0.985 | 1.337 |
| Process CPU | 1.001 | 0.993 | 1.279 |
| Peak RSS | 0.997 | 0.994 | 1.586 |

Seven-cycle rechecks remove initial JIT slowdowns in `fannkuch` (1.068 to 0.969),
generators (1.091 to 1.007), and deque operations (1.055 to 0.977).
Attribute access moves from 1.147 to 1.048, then 1.014 at ten times the work;
its interpreter cost persists at 1.038 after a 1.037 recheck. The fixture named
`fannkuch` exercises sorted-list construction and calls; its flip loop does not
run, so it does not measure canonical fannkuch-redux.

Longer seven-cycle checks give these workload ratios. JSON's initial
interpreter ratio was 1.133 and its recheck 1.058; deque's interpreter recheck
was 1.059. Neither cost persists in these longer runs.

| Longer workload | JIT/base | Interpreter/base |
| --- | ---: | ---: |
| Attribute access, 2 million iterations | 1.014 | 1.038 |
| JSON, 1,500 iterations | 0.998 | 0.959 |
| Deque, 2 million iterations | 1.011 | 0.997 |
| Generators, 3 million iterations | 1.005 | 1.008 |
| Dictionary operations, 1 million iterations | 1.009 | 0.972 |

The numeric-kernel recheck has workload/elapsed/CPU ratios of
1.020/1.056/1.045; its initial ratios were 0.989/0.989/0.979.

The change is retained for repeated import gains, with the interpreter-only
attribute cost left explicit. These results do not establish a general
application throughput improvement.

The 100-worker probe has JIT workload/RSS ratios of 0.995/0.997 and an
interpreter workload ratio of 0.967. JIT work still takes 4.488 times CPython's
time. The executable is 51,814,544 bytes, 48 bytes smaller than the preceding
runtime. Import startup still uses 2.084 times CPython's peak RSS. These results
do not establish overall CPython parity.

An earlier import-budget prototype used separate startup/import flags and a
later lean-call compilation point. It was not adopted: repeated startup costs
and a 4.1% longer dictionary-work slowdown remained. Its binary, source, and
results are retained separately. This version combines the phase state and
preserves the normal relative warm point for callees and loops.

## Validation and records

All 375 VM tests and 201 release regression runs pass across JIT,
interpreter-only, and GIL-disabled modes. Native-state tests prove that short
imports defer compilation, busy imports compile, and later calls can compile
after import exits. Coverage includes nested/circular imports, failed imports,
thread isolation, unwinding, and explicit startup precedence. VM Clippy passes
with the existing local `let_and_return` and socket-alignment exceptions.
Fourteen benchmark-tool tests pass. Seven semantic checks of the portable
first-use probe pass on CPython and both binaries in all three WeavePy modes;
their printed timings are excluded. Nine other candidate probe checks pass.

Source, executable, fixture, and tool hashes are retained with raw samples
under `target/performance/import-compilation-phase-experiment/` and
`target/performance/import-phase-*.json`. The release binary was built with
`cargo build --release -p weavepy-cli --bin weavepy` and copied only after the
build exited successfully. No application recheck replaces the full-suite
geometric means, and no benchmark gate or threshold is changed.
