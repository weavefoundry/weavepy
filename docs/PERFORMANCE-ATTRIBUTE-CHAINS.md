# Borrow intermediate native attribute results

Consecutive native attribute reads used to create or find an owning pin for
each intermediate instance. A chain now walks two to four guarded fields in
one helper and only pins its final result. Stable chains avoid repeated helper
calls and pin searches; replacing a chain no longer retains its discarded
intermediate nodes through those pins.

Lowering combines only adjacent bytecode reads with consecutive guard sites,
within one basic block. Every intermediate must be an instance. Class versions,
storage names and indices, and result lanes remain guarded. A miss replays from
the first read with its original receiver and stack. Successful prefix reads
can't run Python, so replay doesn't repeat observable callbacks.

The helper borrows the graph owned by the root pin without mutating it or
releasing the GIL. An outside-GIL observer can disable guardless reads; slotted
chains then retain scoped borrows through a recursive fallback. Dictionary
reads retain their existing deoptimization behavior when peeking is unavailable.
Final object results retain exact identity checks and the existing pin limit.

## Measurement

The baseline runtime is 78f894b. Later tooling commit 547cfc8 and weakref-test
fixture commit 664040f don't change runtime behavior. Measurements use macOS
x86-64, optimized CPython 3.14.5, alternating paired samples, a discarded
warmup, separate frozen caches, and an otherwise idle host. Builds and tests
finish before timing. Ratios below are medians of paired ratios; lower is better.

Seven-cycle probes give these results. Workload time excludes process startup;
elapsed time, CPU, and peak RSS cover the whole process.

| Probe | JIT workload/baseline | Elapsed/baseline | CPU/baseline | RSS/baseline | JIT workload/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| Four dictionary fields | 0.561 | 0.708 | 0.704 | 1.003 | 0.774 |
| Four slot fields | 0.365 | 0.525 | 0.513 | 0.997 | 0.885 |
| Four mixed dictionary/slot fields | 0.463 | 0.625 | 0.611 | 1.002 | 0.924 |
| Replaced chains with 64 KiB payloads | 0.788 | 0.803 | 0.802 | 0.487 | 2.014 |

Two- and three-field dictionary chains take 0.830 and 0.650 times baseline
workload time. Their CPython ratios are 0.665 and 0.710. Eight-field chains
still take 13.710 times CPython's workload time because deeper provenance
falls back to dynamic lookup; this change doesn't fix arbitrary chain lengths.

Although the four-field read probes beat CPython on workload time, their
process elapsed ratios remain 1.022, 1.063, and 1.094, respectively. Their RSS
ratios remain 1.522, 1.503, and 1.490. The replaced-chain probe still uses 1.541
times CPython's RSS. These results don't establish universal CPython parity.

The unchanged 24-fixture suite, measured over three cycles, has these geometric
means. Workload time covers 23 fixtures; process metrics also include startup.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.005 | 0.999 | 1.091 |
| Process elapsed | 0.996 | 0.993 | 1.331 |
| Process CPU | 0.995 | 0.997 | 1.300 |
| Peak RSS | 0.997 | 0.998 | 1.581 |

All four 31-cycle JIT startup elapsed ratios are within 1% of baseline. The
100-worker probe takes 0.978 times baseline workload time and 0.999 times RSS.

Seven-cycle rechecks don't reproduce the initial Fibonacci JIT, AES interpreter,
JIT-loop, or JSON RSS increases: their repeated ratios are 0.997, 1.001, 1.001,
and 0.995. Call overhead retains a smaller increase, 2.2% with the JIT and 2.5%
without it. Interpreter fannkuch remains 5.2% slower, following a 4.9% increase
in an earlier seven-cycle check. That fixture primarily exercises function calls
and small list/range construction. These residual regressions remain visible;
no cause has been established. Earlier dictionary and attribute interpreter
regressions from a larger helper implementation don't recur in this version.

## Validation

All 64 JIT crate tests and all 367 VM tests pass. VM tests use an 8 MiB thread
stack. Fifty-one release scripts pass with the JIT, without the JIT, and with
the GIL disabled, totaling 153 runs. Clippy passes with the two existing
exclusions used in preceding reports.

The lowering regression verifies optional helper registration, the first-read
deoptimization snapshot, and rejection of nonadjacent bytecode. The VM test
verifies that dictionary, slot, object-result, and string-result drivers compile
and actually use fusion. It then checks mutation, storage reordering,
descriptors and callback counts, missing fields and traceback locations,
nullable intermediates, final-result identity, and suspended generators. An
observer-mode check verifies that native slotted reads use the borrowed fallback.

A weakref regression replaces a chain and collects it inside the same native
activation. The previous runtime retains the intermediate; the candidate and
CPython collect it. The assertion covers the fused four-field case, not the
separate lifetime limitation at the boundary of longer chains.

## Reproduction

Build with cargo build --release -p weavepy-cli --bin weavepy and preserve each
binary before rebuilding. Use tools/bench_compare.py with --samples 7 and
--probe tools/bench_attribute_chains.py --work 1000000 for stable reads, or
--probe tools/bench_attribute_chain_churn.py --work 16384 for replacement.
Set WEAVEPY_STDLIB_CACHE to a staged standard library and provide a fresh
--frozen-cache-root. Standard fixtures, work sizes, CI baselines, and gate
thresholds are unchanged. Raw samples and source snapshots stay under target/.
