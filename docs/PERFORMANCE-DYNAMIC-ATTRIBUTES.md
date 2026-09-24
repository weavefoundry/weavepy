# Cached dynamic attributes and borrowed getter paths

Dynamic attribute reads now reuse the exact instruction's callback-free
lookup paths for ordinary instances, plain classes, and imported modules.
Successful reads avoid owning the attribute name, marking the activation dirty,
and rechecking unrelated global and callee guards. Exact built-in method
capture retains its ordinary binding rules.

Simple properties and ordinary `__getattr__` functions can finish through a
bounded evaluator using borrowed operands. It follows forward branches and
stops before calls, stores, exceptions, unsupported operations, or attribute
overrides. Live getter code, argument binding, recursion depth, observer state,
and free-threaded execution remain guarded. The existing pure-call evaluator
avoids the added getter-specific checks through constant selection.

When a read could invoke Python, native execution exits before that read and
retries it with the original receiver, current locals, and a complete caller
frame. This fixes callbacks seeing stale loop locals during OSR or the module
caller on a subsequent direct native entry. Regression fixtures cover caller
identity and line, writes through `f_locals`, and single execution of raising
callbacks. Instruction-position staging and the interpreter's class-key
callback guard were committed separately in 711aec3 and 8a9c50c.

## Focused measurements

Seven alternating paired cycles compare both WeavePy modes and CPython 3.14.5
on macOS x86-64. Results are medians of paired ratios; lower is better.
The attribute probe reads two names 100,000 times. Warm cases first run an
untimed invocation. The chain probe performs one million iterations, with
construction inside its timer. The baseline is the accepted eight-field runtime.

| Attribute case | JIT time/baseline | JIT RSS/baseline | JIT time/CPython | JIT RSS/CPython |
| --- | ---: | ---: | ---: | ---: |
| class-cold | 0.478 | 1.003 | 3.610 | 1.511 |
| class-warm | 0.478 | 0.967 | 3.378 | 1.503 |
| module-cold | 0.873 | 0.995 | 3.033 | 1.511 |
| module-warm | 0.816 | 0.994 | 2.716 | 1.510 |
| property-cold | 0.309 | 1.006 | 3.213 | 1.563 |
| property-warm | 0.274 | 0.959 | 2.991 | 1.527 |
| getattr-cold | 0.132 | 0.950 | 1.960 | 1.523 |
| getattr-warm | 0.125 | 0.977 | 1.787 | 1.607 |

An earlier uncommitted candidate materialized every getter call. It fixed
frame visibility but made `__getattr__` 19% to 21% slower. Borrowed evaluation
removes that regression. The preceding borrowed-getter candidate and current
candidate agree within 4.1% on the focused JIT timings. Neither getter nor
class/module workload time reaches CPython.

| Chain case | JIT time/baseline | Interpreter time/baseline | JIT RSS/baseline | JIT time/CPython |
| --- | ---: | ---: | ---: | ---: |
| dict-4 | 0.891 | 0.929 | 1.007 | 0.693 |
| dict-8 | 0.955 | 1.054 | 0.997 | 0.847 |
| dict-9 | 0.373 | 0.940 | 0.981 | 1.707 |
| dict-16 | 0.321 | 0.994 | 0.982 | 7.900 |
| slots-16 | 0.309 | 1.037 | 0.919 | 8.621 |
| mixed-16 | 0.258 | 1.031 | 0.977 | 8.341 |

Long dynamic chains still retain owning intermediate pins and remain much
slower than CPython. Object lanes, roundtrip accounting, and pin capacity are
unchanged. This change doesn't attempt general pin-liveness recovery.

## Application, startup, and lifetime measurements

The unchanged 24-fixture suite ran three paired cycles. Workload geometric
means exclude startup; process metrics include it.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.012 | 1.007 | 1.089 |
| Process elapsed | 1.023 | 1.013 | 1.342 |
| Process CPU | 1.022 | 1.010 | 1.301 |
| Peak RSS | 1.000 | 1.004 | 1.581 |

These application averages don't establish an overall speed improvement.
A seven-cycle recheck of every application row with a first-pass increase
above 3% removes the apparent sum-loop, nested-loop, N-body JIT, Richards JIT,
and string-workload slowdowns. JIT workload increases remain for jitloop
(5.3%), jitkernels (3.6%), spectral norm (3.0%), JSON (3.6%), and attribute
access (10.7%). Interpreter increases remain for N-body (4.2%), datetime
(3.5%), and attribute access (13.3%). Pi-digits interpreter RSS remains 5.1%
higher. Some process-only increases remain, including nested-loop JIT CPU
at 7.6%. First-pass and recheck samples both remain in the raw reports.

The mixed sixteen-field recheck takes 0.269 times baseline JIT workload time,
with interpreter time unchanged, retaining the large focused improvement.
An eleven-cycle diagnostic repeats the unchanged attribute workload at ten
times its normal work size. Its JIT workload ratio is 1.018 and interpreter
ratio is 1.046, with JIT RSS unchanged. This candidate takes 1.044 and 1.009
times the earlier borrowed-getter build's time in those two modes in the same
comparison.
The short-run increases are therefore not stable at their original magnitude;
the longer interpreter increase remains. Six-second stack samples locate
about 42% to 43% of active samples in the existing interpreter dispatch loop,
with none in the new unresolved-class guard. These profiles don't establish
a cause for the residual slowdown. This change accepts the focused gains
while retaining those application and startup costs as further work.

Thirty-one paired startup cycles give default-JIT elapsed ratios of 1.027 for
normal startup, 1.015 for isolated startup, 1.068 for `-S`, and 1.038 for the
import probe. Normal startup still takes 1.407 times CPython's elapsed time and
1.226 times its RSS; imports take 2.704 and 2.252 times, respectively. The `-S`
case beats CPython in elapsed time and RSS, at 0.789 and 0.758 times.

Seven-cycle worker and replacement probes retain small costs. The 100-worker
probe has workload/RSS ratios of 1.020/1.013; four-field replacement has
1.040/1.010, and eight-field replacement has 1.018/1.002. Interpreter-only
eight-field replacement takes 4.0% more workload time. The JIT worker workload
still takes 4.716 times CPython's time; replacement workloads take about twice
CPython's time and use about 1.576 times its RSS.

## Validation and provenance

All 68 JIT tests and 371 VM tests pass. Strict JIT Clippy passes. VM Clippy
passes with the existing local `let_and_return` and `cast_ptr_alignment`
exclusions. Fifty-six release scripts pass in JIT, interpreter-only, and
free-threaded modes, for 168 runs. The new fixtures also pass on CPython.
Native counters verify that dynamic field, class, module, and getter paths
actually run. Independent warmed drivers preserve native coverage for later
descriptor-binding, suspended-getter, and colliding-key mutations.

The three new isolated VM regressions also pass without a global
`RUST_MIN_STACK` override. CI exposed the class-key test's need for an explicit
8 MiB worker stack on Linux and Windows; 82e93dc corrects that test setup.
This test-only correction doesn't change the measured release executable.

The class-key frame assertion fails on the preceding runtime even with the
JIT disabled. The interpreter fix alone doesn't fix native callers; the
callback-free dynamic helper completes that correction. An imported module's
custom `__getattribute__` ignoring an existing namespace key was also reproduced
with the baseline JIT disabled. This change rejects re-classed imported modules
before native lookup; it doesn't repair that interpreter protocol limitation.

The baseline binary, `weavepy-deep-attribute-chains`, has SHA-256
`2a2e24417b524be216b802482e97c4ee7587c4f7bbdd74fd02b51862affe4bb1`.
The candidate, `weavepy-guarded-dynamic-attributes`, has SHA-256
`87a2e20c187669d6563553ecf26b0d723e46556ffdb09ebcdfc58ab404e57f24`.
It contains 8a9c50c plus the recorded runtime patch and new fixtures. It is
51,795,256 bytes, 16,840 bytes larger than baseline. The preceding uncommitted
borrowed-getter build is retained separately as `--previous`.

Raw samples, source and binary hashes, isolated frozen-cache manifests, and
validation logs are under `target/performance/guarded-dynamic-*`. Earlier
candidates and their measured regressions remain under `getter-path-*` and
`dynamic-attribute-*`. Generated research isn't committed. Builds, tests,
profiles, and benchmark runs never overlap. Every comparison uses the same
staged standard library and unchanged application work sizes. CPython has PGO,
LTO, and the tail-call interpreter; its experimental JIT is disabled. Launch
context is recorded as sandboxed. No CI gate or benchmark baseline was relaxed.

Build with `cargo build --release -p weavepy-cli --bin weavepy` and preserve
both binaries before comparing them with `tools/bench_compare.py`. Use
`--probe tools/bench_dynamic_attributes.py --work 100000 --samples 7` and set
`WEAVEPY_DYNAMIC_ATTRIBUTE_KIND` to `class`, `module`, `property`, or `getattr`.
Add `--warm` for warmed readings. Supply a staged `WEAVEPY_STDLIB_CACHE` and
fresh `--frozen-cache-root`; keep generated output under `target/`.
