# Compile loops after keyword constructors

A specialized constructor call with keywords used to reject JIT analysis of
its entire function, including later numeric loops. The analyzer now retries
that global as an ordinary guarded object and emits the existing dynamic
keyword-call operation. Normal argument binding, exceptions, caller-frame
visibility, and global mutation checks remain in the call helper.

This also preserves runtime validation when a specialized builtin cannot
accept the supplied keywords. The change adds no new native calling convention.

## Measurement

The baseline is `f67c5ea`. The macOS x86-64 host, CPython 3.14.5 reference,
release profile, alternating paired samples, and frozen-cache isolation match
the [class-cache report](PERFORMANCE-CLASS-CACHES.md). Supplemental probes use
one untimed invocation before timing the second, with 1,000,000 iterations
and seven paired cycles. Their source hashes are recorded with the results.

| Default JIT | Workload time | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| Numeric loop, modified/baseline | 0.056 | 0.374 | 1.027 |
| Attribute chain, modified/baseline | 0.742 | 0.933 | 1.180 |
| Numeric loop, modified/CPython | 0.052 | 0.408 | 1.555 |
| Attribute chain, modified/CPython | 3.350 | 2.861 | 1.790 |

The numeric loop's median workload time falls from 38.80 ms to 2.17 ms,
compared with CPython's 42.61 ms. The attribute-chain probe falls from
177.55 ms to 131.65 ms, while its peak RSS rises from 17.04 MB to 20.10 MB.
Compiling additional functions costs memory; this change does not establish
CPython parity across workloads or memory metrics. Interpreter-only workload
ratios are 1.015 and 1.004, respectively.

The chain probe also exposes repeated object-pin pressure exits. It still
allocates a new pin for each object-valued attribute read, even when successive
iterations read the same objects.

The three-cycle full suite gives the following geometric means. Workload time
covers 23 fixtures; process metrics include startup for 24 fixtures.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.998 | 1.001 | 1.060 |
| Process elapsed time | 0.990 | 0.992 | 1.368 |
| Process CPU | 0.991 | 0.990 | 1.358 |
| Peak RSS | 0.997 | 0.994 | 1.716 |

Seven-cycle focused standard-fixture runs give default-JIT workload ratios
between 0.986 and 1.021 across DeltaBlue, Richards, attribute access, call
overhead, float math, dictionary operations, deque operations, and startup.

Seven-cycle rechecks do not reproduce the initial Richards RSS or
interpreter-startup regressions: their modified/baseline ratios are 0.994 and
0.970, respectively.

## Validation

Three analyzer regressions verify constructor demotion, nested mixed
positional/keyword calls, OSR eligibility, object-global guards, and retention
of invalid builtin keywords for runtime validation. All 37 frame-coverage
tests pass. Traces confirm both supplemental loops compile, with guarded attributes and
the existing dynamic constructor-call operation. The runtime regression checks initializer replacement, global
rebinding, keyword-only and positional-only signatures, argument side effects,
exceptions, caller-frame inspection, and numeric lane changes.

Twenty-two regression scripts pass in default-JIT, interpreter-only, and
free-threaded modes (66 runs). The new runtime regression also passes on
CPython. The release CLI build, formatting, and targeted JIT/VM Clippy pass.
Clippy uses the same two preexisting local exclusions as preceding reports.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/f67c5ea/weavepy --new target/release/weavepy \
  --probe tools/bench_keyword_setup.py --work 1000000 --warm --samples 7 \
  --frozen-cache-root target/performance/fresh-keyword-caches \
  --out target/performance/keyword-setup.json
```

Repeat with `tools/bench_attribute_chains.py` and fresh cache/output paths for
the linked-instance probe. Raw results remain under
`target/performance/keyword-callee-*`. Standard fixtures, work sizes, CI
baselines, and gate thresholds are unchanged.
