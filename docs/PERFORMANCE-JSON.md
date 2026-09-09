# JSON and string performance

This pass measures changes against release commit `33c211b` and CPython
3.14.7 on macOS ARM64. It doesn't establish that WeavePy is faster than
CPython for every workload or metric.

## Standard suite

Five paired cycles retain all 24 existing fixtures and their original work
parameters. The timed-workload aggregate excludes only startup; it includes
the deque, datetime, and pickle census fixtures. All other aggregates include
all 24 fixtures. Each fixture has equal weight in the geometric mean. Values
below 1 indicate less time or memory.

| Metric | New/base JIT | New/base interpreter | New/CPython JIT | New/CPython interpreter |
| --- | ---: | ---: | ---: | ---: |
| Timed workload, 23 fixtures | 0.940 | 0.940 | 4.117 | 9.522 |
| Whole-process elapsed time | 0.958 | 0.957 | 3.637 | 5.783 |
| Whole-process CPU time | 0.958 | 0.957 | 3.727 | 5.980 |
| Peak RSS | 0.999 | 1.000 | 2.194 | 2.028 |

JSON roundtrip time falls by 73.0% with the JIT and 72.8% in the
interpreter. Its whole-process CPU time falls by 59.9% and 60.4%, respectively.
Most unrelated paths stay close to baseline;
the aggregate improvement comes mainly from JSON. The numerical JIT wins
over CPython already existed before this pass.

| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |
| --- | ---: | ---: | ---: | ---: |
| `fannkuch` | 0.991 | 1.003 | 10.748 | 2.064 |
| `nbody` | 0.986 | 0.986 | 8.819 | 2.061 |
| `fib` | 1.008 | 0.995 | 2.999 | 2.089 |
| `pidigits` | 0.998 | 0.996 | 0.888 | 2.067 |
| `pyaes` | 0.989 | 0.987 | 12.050 | 2.031 |
| `richards` | 1.002 | 1.002 | 7.736 | 2.073 |
| `sumvm` | 0.999 | 1.014 | 0.058 | 2.085 |
| `nested_loops` | 1.001 | 1.004 | 0.097 | 2.087 |
| `jitloop` | 1.003 | 1.000 | 0.073 | 2.085 |
| `jitkernels` | 1.001 | 0.984 | 0.884 | 2.072 |
| `deltablue` | 0.986 | 0.995 | 19.830 | 2.269 |
| `float_math` | 1.010 | 1.003 | 8.416 | 3.481 |
| `spectral_norm` | 1.001 | 0.998 | 2.596 | 2.082 |
| `json_bench` | 0.270 | 0.272 | 1.502 | 2.760 |
| `str_methods` | 0.969 | 0.962 | 3.139 | 2.175 |
| `dict_ops` | 0.983 | 0.985 | 5.973 | 2.046 |
| `list_ops` | 0.989 | 0.998 | 13.917 | 2.059 |
| `attr_access` | 1.004 | 0.997 | 3.747 | 2.202 |
| `call_overhead` | 0.991 | 0.993 | 8.136 | 2.080 |
| `generators` | 0.993 | 0.997 | 9.666 | 2.093 |
| `deque_ops` | 0.994 | 0.988 | 28.400 | 2.125 |
| `datetime_ops` | 1.002 | 1.024 | 31.621 | 2.251 |
| `pickle_bench` | 1.000 | 0.977 | 356.543 | 2.689 |
| `startup` | 0.985 | 0.985 | 1.361 | 2.077 |

The startup row measures process elapsed time. Other fixture timers include
first-entry JIT costs but exclude process initialization. Datetime and pickle
still rely on Python implementations; deque uses a Python class with native
end operations. These remain substantial performance gaps.

[Raw samples, environment, and validation results](../crates/weavepy-bench/census/2026-09-json/)
retain every recorded metric, including both execution modes and per-process
CPU time.

## Large inputs

Seven paired cycles of the supplemental probes give the following results.
Times are marginal medians for the timed workload, in milliseconds. Peak RSS
is for the whole process, in MiB, including imports and input construction.
These probes disable the JIT and time one invocation of `bench()` per
process. The baseline and modified executable use identical release flags.

| Workload | Baseline time | New time | CPython time | Baseline RSS | New RSS | CPython RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Prefix and suffix checks | 204.75 | 0.0208 | 0.0051 | 2,172.48 | 35.69 | 17.78 |
| Bounded whitespace rsplit | 19.69 | 0.3226 | 0.6008 | 235.73 | 32.64 | 21.42 |
| Positive bounds | 298.25 | 0.0286 | 0.0067 | 3,240.97 | 35.66 | 17.81 |
| Negative bounds | 195.20 | 0.0219 | 0.0050 | 2,172.47 | 35.67 | 17.83 |
| Encode records | 160.10 | 39.43 | 25.69 | 45.19 | 40.44 | 16.00 |
| Decode records | 102.19 | 29.16 | 26.84 | 46.73 | 42.50 | 16.86 |
| Encode large ASCII string | 94.14 | 40.77 | 65.47 | 244.86 | 73.02 | 40.47 |
| Decode large ASCII string | 199.07 | 34.64 | 38.88 | 854.50 | 98.78 | 40.52 |
| Encode Unicode | 264.57 | 75.28 | 78.70 | 60.48 | 53.95 | 67.56 |
| Decode Unicode | 44.49 | 19.97 | 11.55 | 120.58 | 46.86 | 26.20 |
| Decode numeric array | 105.05 | 8.90 | 19.42 | 44.81 | 41.41 | 16.91 |
| Decode with custom hooks | 27.44 | 27.19 | 1.52 | 38.58 | 38.64 | 14.67 |
| Encode lone surrogates | Invalid | 13.29 | 19.00 | Invalid | 73.30 | 23.27 |
| Decode lone surrogates | 23.01 | 15.59 | 7.41 | 68.84 | 54.36 | 18.89 |

The paired ratios show 4.07 times faster record encoding, 3.53 times faster
record decoding, and 11.85 times faster numeric-array decoding. Large ASCII
decoding uses 88.4% less peak RSS; Unicode decoding uses 61.1% less. Bounded
whitespace splitting is 60.8 times faster. The prefix probe demonstrates the
change from scanning the whole input to checking just the needle; its new
microsecond timings are too short to use as a general interpreter speedup.

Several timed kernels now beat this CPython build: bounded splitting, large
ASCII encoding and decoding, Unicode and surrogate encoding, and
numeric-array decoding.
Their complete process runs still take longer than CPython's. Unicode
encoding is the only supplemental probe that also uses less peak RSS than
CPython. Custom callbacks remain much slower because they execute Python
code. These distinctions matter when choosing an interpreter for a service
or a short command.

The surrogate encoder's baseline fails the unchanged workload assertions:
its result drops lone surrogates inside a container. No baseline time or
memory ratio is reported for invalid output. The candidate preserves the
surrogates and passes the same assertions as CPython. These two extra probes
were measured in a separate interleaved run recorded in `surrogates.json`;
`probe-baseline-error.txt` retains the original failed run.

The earlier UTF-8 JSON candidate made Unicode-decoding RSS worse because a
separate prefix check still allocated an index for the entire input. That
candidate wasn't retained. Its measurements live in `before-prefix/` so the
memory regression and its correction remain visible.

## Implementation

The JSON encoder writes directly into an output buffer instead of building
and then joining a list of temporary Python strings. It writes UTF-8 runs
without first expanding each string into code points. The buffer widens when
it encounters an unescaped surrogate, preserving strings that Rust's UTF-8
representation can't hold. This also fixes previously dropped surrogates in
nested string values, indentation, separators, and custom encoder results.

Native string encoding is selected by the original function's identity.
Replacing a module attribute or passing a custom encoder still invokes the
custom callable. List iteration remains live so callbacks can mutate the
list during encoding. Custom defaults, sorting, circular-reference checks,
recursion checks, and serialization error notes retain their existing paths.

Unbounded `str.startswith()` and `str.endswith()` calls no longer build an
index of every character. Bounded calls find their byte boundaries without
allocating that index. Positive bounds scan from the front and negative
bounds scan from the back, without counting the entire string. This matters
for `json.loads()`, which checks the entire input for a BOM with
`startswith()`. Bounded whitespace `rsplit()` also scans directly from the
right and allocates only its result pieces.

The decoder parses UTF-8 documents from their existing bytes. It converts
byte offsets to Python character offsets only at the public boundary and
when constructing an error. Documents containing lone surrogates use the
same parser instantiated for Unicode code points. Strings without escapes
can be constructed directly from the input slice. The key memo retains immutable
string objects instead of allocating another code-point array for every key.
Exact built-in integer and float parsers have direct numeric paths; custom
parsers still receive the original token. Large integers use the existing
conversion path, including the configured digit limit.

Invalid-escape errors now point at the backslash, matching CPython's native
scanner. CPython's pure-Python scanner reports the following character for
this particular error, so the differential checks use the native oracle.

An operand-path experiment separated rare stack-underflow and local-variable
error construction from the successful path, then forced inlining of pops
and local loads. Across eight warmed fixtures and seven measured cycles,
the geometric mean improved by only 0.5% in each execution mode. List
operations regressed by 2.8% with the JIT and 2.5% without it. The experiment
was removed. Its patch and measurements are retained for reproducibility;
it isn't part of the final implementation.

The benchmark comparison tool now reports paired WeavePy/CPython ratios for
every recorded metric in both execution modes. This includes elapsed time,
CPU time, and peak resident memory. An elapsed-time win doesn't imply a CPU
or memory win. Supplemental JSON probes separate encoding and decoding and
cover records, large strings, Unicode, numbers, and custom hooks.

## Startup and compilation

Separate probes use 31 measured cycles for ordinary startup, no-site startup,
and imports; compilation and fresh-cache extraction use five. All discard an
initial process cycle. Times below are marginal medians. Startup and imports
measure complete processes; compilation measures the timed `compile()` calls.

| Workload | Baseline ms | New ms | CPython ms | Paired new/base |
| --- | ---: | ---: | ---: | ---: |
| Startup | 30.08 | 30.12 | 22.47 | 0.989 |
| Startup without site | 9.79 | 9.78 | 15.73 | 1.000 |
| Import four standard modules | 74.42 | 74.30 | 27.23 | 0.993 |
| Compile a small snippet 1,500 times | 73.92 | 74.08 | 69.94 | 1.013 |
| Compile 20,000 assignments | 40.36 | 40.44 | 50.67 | 1.040 |
| Startup with a fresh library cache | 150.96 | 148.45 | N/A | 0.999 |

This pass makes no startup or compilation improvement claim. The compiler
probe varies by about 4% in this run, while the preceding full run was close
to baseline. Fresh-cache extraction uses a new library-cache directory; it
does not flush the operating system's file cache.

## Threads

The existing eight-thread fixture uses 3,000,000 iterations per worker. Both
WeavePy modes request `WEAVEPY_JIT=1`; runtime gating still applies. The
initial five-cycle run alternated GIL modes. Its GIL-enabled serial result
appeared 15.7% slower, and process CPU appeared 23.5% worse. The slow samples
occurred immediately after the much heavier free-threaded processes.

A seven-cycle follow-up groups each GIL mode separately, still interleaving
the binaries and discarding the first pair. GIL-enabled serial and parallel
times are within 0.2% of baseline. This supports an order effect in the mixed
run. Both runs are retained, in `scaling.json` and `scaling-isolated.json`.

| Isolated mode | New/base serial | New/base parallel | New/base process CPU | New/base RSS |
| --- | ---: | ---: | ---: | ---: |
| GIL enabled | 0.999 | 0.998 | 0.998 | 1.004 |
| GIL disabled | 0.971 | 0.956 | 0.981 | 1.000 |

The separate CPython reference run gives these observed marginal medians.
Its variants also alternate modes, so the absolute times are subject to the
same order sensitivity. Scaling is the median within-process serial/parallel
ratio; it need not equal the ratio of the displayed marginal medians.

| Runtime | Serial ms | Parallel ms | Scaling | Peak RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| WeavePy, GIL enabled | 146.04 | 141.50 | 0.94 | 60.77 |
| WeavePy, GIL disabled | 4092.84 | 1656.82 | 2.47 | 55.97 |
| CPython, installed GIL build | 1181.85 | 1102.86 | 1.07 | 15.03 |

Better scaling does not imply faster execution: the free-threaded WeavePy
case scales across workers but still takes longer than the installed CPython
build. Its paired process CPU ratio is 6.02, and its RSS ratio is 3.72. The
GIL-enabled WeavePy case is faster on this numerical kernel but uses about
four times CPython's RSS. This pass makes no general threading-performance
claim, and a free-threaded CPython build wasn't tested.

## Scope and limits

Small percentage differences on unchanged paths are inconclusive. Paired
samples reduce host-speed drift but don't eliminate scheduling noise or
changes in CPU frequency. Peak RSS counts resident pages, including pages
retained by the allocator; it isn't a count of live objects or allocations.

The executable grows from 44,129,472 to 44,166,432 bytes, an increase of
36,960 bytes (0.084%). This pass doesn't improve binary size. Energy,
controlled Rust build time, allocation counts, service tail latency, other
platforms, and alternative CPython builds weren't measured. Neither isolated
kernel wins nor the aggregate benchmark score establishes superiority across
all meaningful metrics.

## Reproduction

Build the baseline from a clean checkout of `33c211b`, retain the executable,
apply the changes, and build the CLI again. The CLI package owns the
`weavepy` binary:

```sh
cargo build --release -p weavepy-cli --bin weavepy
cp target/release/weavepy target/release/weavepy-perf-33c211b
# Apply the source changes, then rebuild.
cargo build --release -p weavepy-cli --bin weavepy
python3.14 crates/weavepy-bench/census/2026-09-json/verify.py \
    --weavepy target/release/weavepy
python3.14 crates/weavepy-bench/census/2026-09-json/validate.py
python3.14 tools/bench_compare.py \
    --base target/release/weavepy-perf-33c211b --new target/release/weavepy \
    --samples 5 --out target/json-suite.json
python3.14 crates/weavepy-bench/census/2026-09-json/probes.py \
    --base target/release/weavepy-perf-33c211b --new target/release/weavepy \
    --samples 7 --out target/json-probes.json
python3.14 crates/weavepy-bench/census/2026-09-execution/probes.py \
    --base target/release/weavepy-perf-33c211b --new target/release/weavepy \
    --samples 5 --only startup startup_no_site imports compile_small \
    compile_20000 startup_fresh_cache --out target/json-startup-compile.json
python3.14 crates/weavepy-bench/census/2026-09-execution/scaling.py \
    --base target/release/weavepy-perf-33c211b --new target/release/weavepy \
    --samples 5 --work 3000000 --out target/json-scaling.json
python3.14 crates/weavepy-bench/census/2026-09-json/scaling-cpython.py \
    --weavepy target/release/weavepy --samples 5 --work 3000000 \
    --out target/json-scaling-cpython.json
python3.14 crates/weavepy-bench/census/2026-09-json/scaling-isolated.py \
    --base target/release/weavepy-perf-33c211b --new target/release/weavepy \
    --samples 7 --work 3000000 --out target/json-scaling-isolated.json
```

Finish compilation and correctness tests before timing. Each measurement
cycle interleaves the interpreters, reversing their order on alternate
cycles. One initial cycle is discarded. Reports retain all measured samples;
ratios are the median of paired sample ratios. Workload timers exclude
startup; process CPU and peak RSS include initialization. The existing suite
keeps all 24 fixtures and their original work parameters.

The validation driver runs from the repository root and writes its full
results to `target/performance-validation.json`. Its socket and subprocess
fixtures require localhost access. The final run passes all 104 checks:
87 execution fixtures, three execution modes for each of the JSON and string
regression files, the seeded CPython oracle comparison, and ten CPython
regression gates.
Each oracle mode checks 250 encodings, 281 decodings, and 219 error positions.
The modes are the default JIT, the interpreter, and `-X gil=0`.

The ten CPython gates are `test_json`, `test_scope`, `test_frame`,
`test_exceptions`, `test_generators`, `test_sys_settrace`, `test_sys_setprofile`,
`test_gc`, `test_str`, and `test_userstring`; all report zero unexpected
results. Clippy passes for the VM with all features and warnings denied.
The build check without the JIT and
all six benchmark-reporting tests pass. This is targeted validation, not a
complete rerun of the vendored CPython or ecosystem suites.
