# Checked integer lowering: rejected experiment

Replacing the JIT's explicit signed-overflow tests with Cranelift's
`sadd_overflow`, `ssub_overflow`, and `smul_overflow` reduces compilation
work and generated code size, but makes several integer loops slower.
The runtime change was rejected. The original lowering remains in place;
the arithmetic and deoptimization regression tests are retained.

## Measurements

Measurements use macOS x86-64, the runtime at `4ae77f8`, and optimized CPython
3.14.5. Production comparisons alternate process order, preserve each
binary's warmed frozen cache, and run without concurrent builds, tests,
profiles, or other benchmarks. Lower candidate/base ratios are better.

Seven paired cycles give the unchanged jitloop fixture workload ratios of
0.760 cold and 0.700 warm. Fib costs 1.030 cold and 1.022 warm. The three-cycle
application suite gives JIT geometric means of 0.979 workload, 0.994 whole-process
elapsed time, 0.995 CPU, and 0.998 peak RSS. Workload means cover 23 fixtures;
process means cover all 24. These aggregate gains don't establish a general
improvement.

A separate compiler diagnostic compiles and executes 13 integer kernels over
15 paired cycles. It retains a no-op polling call, so it isn't a replacement
for production VM measurements. First compilation improves 2-9%, but execution
regresses by about 25% for while_sum, negative, and descending, 19% for
alternating, and 30% for dependent. The sampled generated functions shrink
11-56 bytes. Generated x86-64 code materializes overflow with `seto` and tests
the result before branching. Smaller code doesn't guarantee a faster
dependency chain; the assembly alone doesn't isolate every regression's cause.

An additional diagnostic tests all eight combinations of old and new lowering
inside one executable. It rotates and reverses strategy order over 15 measured
cycles after warmup. This removes between-executable layout as a confounder.
Every combination containing a new operation has a material execution cost:

| New operation alone | Example regression | Execution ratio |
| --- | --- | ---: |
| Addition | while_sum | 1.251 |
| Subtraction | descending | 1.128 |
| Multiplication | sum_squares | 1.111 |

No benchmark-specific selection rule or benchmark-gate change was introduced.

## Retained coverage and reproducibility

The native-code oracle checks 3,939 addition, subtraction, and multiplication
cases against i128 arithmetic. It checks successful values and tags, unchanged
locals, and the exact pre-operation operands, tags, stack depth, and resume PC
on overflow. Python coverage checks arbitrary-precision continuation, loop
entry through OSR, subsequent calls, booleans, and overloaded operators.
An isolated VM test requires actual native entries, OSR entries, and exits.
With the original runtime lowering restored, all 380 VM tests and the full
JIT suite pass. Clippy passes with warnings denied, retaining the VM's two
existing lint exclusions; formatting and the no-default-features check pass.
The Python fixture passes on CPython and the accepted release binary with
the JIT enabled, disabled, and the GIL disabled.

Research inputs, immutable binaries, raw timings, code dumps, and validation
logs are under `target/performance/checked-integer-codegen-investigation/`
and `target/performance/integer-overflow-strategies-investigation/`.
The rejected production binary SHA-256 is
`8f38605d3e91bb86bed7f1db7368bf949ef54e76a624f69112957c2598f60436`.
The eight-strategy diagnostic passes all 512 kernel/input/strategy checks.
