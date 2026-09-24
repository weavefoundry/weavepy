# Short first range calls

A first invocation can spend more time compiling a small scalar loop than it
would spend finishing in the interpreter. The default JIT policy now estimates
remaining work from a live built-in range iterator. Below 65,536 remaining
bytecode instructions, a single straight-line scalar range loop defers OSR.
It retains accumulated hotness and clears the deferral at a fresh call, so
repeated use can compile. No permanent not-jitable hint is set.

The estimate applies only to one matching loop/backedge, step +1, an exact
range iterator, scalar local/constant loads, local stores, and addition,
subtraction, or multiplication. Nested loops, unknown iterators, calls in the
body, exception handlers, and generators keep the existing policy. Arithmetic
uses checked work calculations. A valid explicit `WEAVEPY_JIT_THRESHOLD`
retains its previous behavior and bypasses this heuristic. The test-only
force-enable hook does the same; a separate isolated test covers the default.

## Measurements

Measured against `14b49e2` on macOS x86-64 and optimized CPython 3.14.5
(PGO, LTO, tail-call interpreter, experimental JIT off). Runs alternate order,
discard a warmup cycle, and verify nonempty per-binary frozen caches stay
unchanged. No build, test, profile, or other benchmark overlaps timing.
Lower ratios are better.

The new probe defines 32 distinct functions. Its timer includes source
construction, compilation, all calls, and result checks. Seven paired cycles
per case, with iterations per call and calls per function shown separately:

| Iterations | Calls | JIT/base time | JIT/base elapsed | JIT/base CPU | JIT/base RSS | JIT/CP time | JIT/CP RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 1 | 0.133 | 0.777 | 0.748 | 0.906 | 1.197 | 1.301 |
| 500 | 1 | 0.175 | 0.796 | 0.754 | 0.900 | 1.195 | 1.291 |
| 2,000 | 1 | 0.309 | 0.838 | 0.811 | 0.901 | 1.195 | 1.299 |
| 8,000 | 1 | 0.840 | 0.939 | 0.967 | 0.902 | 1.245 | 1.298 |
| 20,000 | 1 | 1.016 | 0.997 | 1.017 | 1.010 | 0.648 | 1.449 |
| 200,000 | 1 | 0.962 | 0.974 | 0.967 | 1.005 | 0.112 | 1.453 |
| 2,000 | 2 | 1.181 | 1.050 | 1.075 | 1.014 | 3.010 | 1.458 |
| 2,000 | 8 | 1.152 | 1.041 | 1.073 | 1.023 | 0.994 | 1.463 |

One-call cases at 100-2,000 iterations improve about 3.2-7.5 times, with
about 10% lower peak RSS. At 8,000 iterations, the gain is 16%. Long loops
still compile. The first-call gain has a cost: repeated short calls execute
the first invocation in the interpreter, then pay for compilation on the next
call. The initial two-call and eight-call checks cost 18% and 15% workload
time, respectively. The 65,536-instruction budget is a heuristic, not a
universal break-even guarantee.

The unchanged 100-worker probe improves to 0.338 times baseline workload
time, 0.378 times process elapsed time, 0.355 times CPU, and 0.967 times peak
RSS. It still takes 1.538 times CPython's workload time, 1.580 times elapsed,
1.578 times CPU, and 1.849 times RSS. Separate traces show 20 worker kernel
compilations become zero; repeated and long 32-function probes still compile
all 32 kernels. Trace timings are excluded.

The unchanged 24-fixture application suite uses three paired cycles. Workload
time excludes startup; process means include all 24 fixtures:

| Metric | JIT/base | Interp/base | JIT/CP | Interp/CP |
| --- | ---: | ---: | ---: | ---: |
| Workload time | 1.001 | 0.997 | 1.064 | 2.228 |
| Elapsed | 0.999 | 0.990 | 1.347 | 1.958 |
| CPU | 0.998 | 0.989 | 1.272 | 1.918 |
| Peak RSS | 1.004 | 1.004 | 1.590 | 1.364 |

## Costs and rechecks

Initial full-suite costs include JIT Fibonacci 1.071, sumvm 1.043, deque
1.076, and generators 1.026 times baseline. Interpreter string methods cost
1.060. Pickle RSS is 1.036 times baseline. These results are retained alongside
the repeated checks below.

Seven-cycle rechecks leave most application ratios near baseline:

| Fixture | JIT/base | Interp/base | JIT/base RSS |
| --- | ---: | ---: | ---: |
| fib | 1.019 | 1.014 | 1.013 |
| sumvm | 0.914 | 0.979 | 1.011 |
| jitloop | 0.990 | 1.004 | 1.010 |
| jitkernels | 0.991 | 1.004 | 1.008 |
| str_methods | 0.995 | 0.990 | 1.004 |
| generators | 0.991 | 1.001 | 1.008 |
| deque_ops | 1.006 | 1.006 | 1.005 |
| pickle_bench | 0.978 | 1.009 | 1.009 |

Seven-cycle longer runs retain the following ratios:

| Fixture | Work | JIT/base | Interp/base | JIT/base RSS |
| --- | ---: | ---: | ---: | ---: |
| sumvm | 20,000,000 | 0.973 | 1.003 | 1.006 |
| jitloop | 3,000 | 1.003 | 0.999 | 1.007 |
| deque_ops | 1,000,000 | 1.000 | 1.000 | 1.004 |
| generators | 500,000 | 1.017 | 0.990 | 1.009 |

The repeated-call costs persist: the second seven-cycle checks give 1.224
for two calls and 1.132 for eight calls. Their process CPU ratios are
1.131 and 1.071, respectively. The first-call improvement and this cost are both retained.

Two independent 31-cycle startup sweeps give these candidate/base ratios:

| Mode | JIT elapsed 1 / 2 | JIT CPU 1 / 2 | Interp elapsed 1 / 2 | Interp CPU 1 / 2 |
| --- | ---: | ---: | ---: | ---: |
| pass | 1.008 / 1.028 | 1.008 / 1.021 | 0.979 / 0.977 | 0.976 / 0.993 |
| no_site | 1.006 / 1.004 | 1.016 / 1.029 | 0.972 / 0.972 | 0.940 / 0.903 |
| isolated | 1.018 / 1.000 | 1.036 / 1.005 | 0.980 / 0.980 | 0.980 / 0.979 |
| imports | 1.010 / 1.006 | 1.020 / 1.007 | 0.978 / 0.977 | 0.977 / 0.980 |

JIT startup costs remain: normal elapsed is 0.8-2.8% higher, and no-site CPU
is 1.6-2.9% higher. JIT RSS is about 0.1-1.5% higher across these modes.
Interpreter startup elapsed improves about 2-3%. Ordinary startup still takes
about 1.51 times CPython's elapsed time and 1.25 times its RSS in the second
sweep; imports take 2.46 times elapsed and 2.09 times RSS. No-site startup
remains ahead on elapsed, CPU, and RSS.

The large worker and first-use gains justify this step; the repeated-call,
startup, and memory costs remain work to address. Application throughput is
approximately unchanged overall.

## Validation and limits

All 377 VM tests pass, including the isolated default-policy test. It proves
short first calls compile nothing, second calls compile, later long calls
remain eligible, and long/nested/while loops execute native code. Explicit
threshold coverage remains enabled. VM Clippy passes with the existing
`let_and_return` and `cast_ptr_alignment` exclusions; formatting passes.

All 77 release regression scripts pass in default JIT, interpreter-only, and
GIL-disabled modes (231 runs), plus twelve scalar probes, six compilation
probes, and nine first-range probes. The new fixture matches CPython and the
baseline for changed range bindings, exact iterator callbacks, negative and
wide ranges, changed operand types, recursion, and tracing. The worker-exit
correctness fixture adds a second short call to preserve native coverage under
the new policy. Its five additional parity runs pass; a separate trace verifies
eight workers and the main thread each compile the shared scalar function.
The worker benchmark's work remains unchanged.

This is a first-use optimization with a repeated-call tradeoff. It does not
establish universal CPython parity or fix the unresolved Windows cold-JIT gate.
No benchmark fixture, work size, CI baseline, or gate threshold changes.

The candidate executable SHA-256 is
`65fadbedd4e3b5317aaadec46bf1000d8e115748858f1e5a7c31f066e42940d8`;
the baseline is
`895a45f943d44164806f9e74c306cfbb889b1c56692af62532875d168998ac37`.
The candidate is 51,819,168 bytes, 128 bytes larger. Both use
`cargo build --release -p weavepy-cli --bin weavepy`. The candidate was copied
only after its successful build exited and source hashes matched. Raw samples
are under `target/performance/range-budget-*.json`; source hashes, validation,
trace counts, and controllers are under
`target/performance/numeric-loop-budget-experiment/`.
