# Callback-free set removal calls

Exact `set.pop` and scalar `set.remove` now use the existing leaf builtin dispatcher,
avoiding generic attribute resolution and call setup. Pop transfers the stored owner.
Remove defers Python equality during its probe and requires both the lookup and the
matched stored value to be leaves. Subclasses, callbacks, observers, and unsupported
arguments retain the full call path. Object layouts and JIT admission settings stay
unchanged.

The stored-value guard matters. A held first candidate admitted integer and string
subclasses that compared equal through their native payload. Their finalizers ran
after the removing function returned and observed the wrong caller. The regression
passes CPython and the accepted runtime, fails that candidate in both GIL execution
modes, and passes the guarded implementation. That rejected source was never timed.

## Measurements

Comparisons use accepted `4c39d55`, Intel macOS, and optimized CPython 3.14.5 with
PGO, LTO, and the tail-call interpreter. Process order alternates, warmup is discarded,
and setup, result checks, and normal collection remain timed. Ratios are paired
candidate/baseline medians; lower is better. All owned builds, tests, and profiles
finished before timing, and timing controllers ran sequentially. The shared desktop
is not a dedicated idle host; its pre-selection snapshot includes Storage at 20%
and a Codex renderer at 33% CPU, with mediaanalysisd idle.

Seven cycles at each size confirm the initial five-cycle selection:

| Workload | Elements | JIT work | Interpreter work |
| --- | ---: | ---: | ---: |
| Pop | 1,000 | 0.551 | 0.499 |
| Pop | 10,000 | 0.494 | 0.498 |
| Pop | 100,000 | 0.494 | 0.523 |
| Remove | 1,000 | 0.598 | 0.513 |
| Remove | 10,000 | 0.500 | 0.505 |
| Remove | 100,000 | 0.525 | 0.524 |
| Remove/add churn | 1,000 | 0.692 | 0.684 |
| Remove/add churn | 10,000 | 0.654 | 0.633 |
| Remove/add churn | 100,000 | 0.685 | 0.673 |

At 100,000 elements, JIT process elapsed/CPU ratios are 0.697/0.663 for pop,
0.721/0.690 for remove, and 0.778/0.760 for churn. Pop and remove RSS stay within
0.2% of baseline. An eleven-cycle churn recheck retains 0.683/0.668 JIT/interpreter
work and 0.915 JIT RSS; its initial interpreter RSS reduction does not repeat
(0.914 initially, 0.997 on recheck).

Discard, difference-update, and weak-set cleanup have no established general gain.
The initial 10,000-element interpreter discard cost of 3.8% falls to 0.8% on recheck.
The 100,000-element difference-update interpreter ratio changes from 0.944 to 1.022.
Weak-set JIT RSS changes from 1.031 to 1.008; rechecked work is 0.998/1.000.
These four focused rechecks use eleven cycles each.

The unchanged 24-fixture suite uses three cycles. JIT geometric means versus baseline
are 0.9987 work, 0.9778 process elapsed, 0.9866 CPU, and 1.0077 peak RSS; interpreter
means are 1.0005, 0.9903, 0.9914, and 1.0028. Work excludes the startup-only fixture.
Eighteen applications are repeated for eleven cycles: every initial movement beyond
3% in either direction on any metric, plus call, float, and nested-loop controls.
Initial interpreter JSON/dictionary/call costs and JIT list RSS do not repeat.
Retained recheck costs include JIT n-body work at 1.023, Fibonacci at 1.029, and
interpreter sumvm at 1.031, although their process elapsed is at or below baseline.
Dictionary JIT work/elapsed/CPU are 1.024/1.021/1.023, following initial near-baseline
results. These costs and variation preclude a general application speedup claim.

Two 31-cycle batches cover all four startup cases. Rechecked JIT elapsed ratios are
0.992 ordinary, 0.986 without site, 0.986 isolated, and 0.988 imports; CPU stays
within 1.3% and RSS within 0.8% of baseline. Interpreter no-site CPU is 0.921 then
0.923. Ordinary JIT startup still takes 1.673 times CPython elapsed.

CPython parity is not achieved. The full-suite JIT geometric means versus CPython
are 1.013 work, 1.372 elapsed, 1.239 CPU, and 1.563 RSS. At 100,000 elements, pop,
remove, and churn still take 2.20, 2.39, and 2.40 times CPython work. Rechecked
weak-set cleanup takes 41.87 times its work and 6.32 times its RSS.

## Validation and provenance

The change passes 404 VM tests, C API set integration, embedding's explicit 1 MiB
stack case, VM Clippy with the two established exclusions, no-default compilation,
formatting, artifact checks, and fourteen benchmark-tool tests. The frozen CLI passes
336 runs over 112 fixtures in three modes, 1,048 semantic checks, twenty-five CPython
fixtures, 180 ordinary-population and 100 GC-population checks, and 120 checks each
for weak-reference access, saved calls, and set mutations.

All 27 selected upstream call/descriptor/GC/set/weakset runs pass without unexpected
exception output. Each mode runs 630 set tests, 57 GC tests with twelve skips, and
46 weak-set tests. Full `test_weakref` was not rerun for this increment. Compilation
decisions match in 53 probes, three applications, and nine population modes.
Instrumented tests establish ordinary fast-path use; warmed heap-return and discarded
pop-result probes preserve owners and finalizer frames while avoiding generic pop
attribute resolution.

The frozen CLI is 51,876,496 bytes, 128 more than baseline. Its SHA-256 is
`3ef740071d630379c50929d2ca9657c5546e2002658cc6d6fb0251ea768250c9`;
the runtime patch against `4c39d55` is
`461cf1655c7862e211539dd0974f4c3750951f77edc713d71737cee9eaf2dd50`.
Immutable sources, binaries, logs, controllers, and raw samples remain under
`target/performance/set-leaf-dispatch-guarded-investigation/` and
`target/performance/set-leaf-calls-guarded-r4c39d55-*`. The rejected first candidate
is retained separately under `target/performance/set-leaf-dispatch-investigation/`.
