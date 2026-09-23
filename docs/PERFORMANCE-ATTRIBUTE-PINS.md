# Reuse pins for repeated native attribute results

A compiled attribute read previously appended a new owning pin for every
object, string, or bytes result. Repeatedly reading the same linked objects
could fill the temporary-pin budget and force reconstruction at loop polls.

Each attribute guard now keeps one advisory pin index. A hit checks the
current activation's table and requires exact allocation identity before
reusing that pin. A miss appends a pin as before. The ordinary class-version,
attribute-name, storage, and lane checks still run before reuse.

The hint owns no object. Nested calls, later activations, and suspended
generators can overwrite it safely because every access validates the current
table and current attribute value. Different objects with equal contents never
share a result pin. Changing-result traversals retain the existing pin limits
and retirement rules.

## Measurement

The baseline is `c43b59d`. The macOS x86-64 host, CPython 3.14.5 reference,
release build, paired sampling, and frozen-cache isolation match the
[keyword-constructor report](PERFORMANCE-KEYWORD-CONSTRUCTORS.md).

Seven paired cycles of the warmed probes give these ratios. Each probe runs
1,000,000 iterations; its untimed first invocation and timed second invocation
both check their result. The text probe reads a retained instance's string and
bytes attributes. The linked-instance probe constructs a four-node chain.

| Default JIT | Workload time | Process CPU | Peak RSS |
| --- | ---: | ---: | ---: |
| Linked instances, modified/baseline | 0.392 | 0.481 | 0.859 |
| String/bytes attributes, modified/baseline | 0.465 | 0.580 | 0.876 |
| Linked instances, modified/CPython | 1.307 | 1.376 | 1.571 |
| String/bytes attributes, modified/CPython | 0.659 | 0.846 | 1.555 |

Median linked-instance workload time falls from 131.46 ms to 51.50 ms and
peak RSS from 20.38 MB to 17.60 MB. Text-attribute time falls from 79.86 ms
to 37.17 ms and RSS from 19.98 MB to 17.44 MB. Interpreter-only workload
ratios are 1.005 and 0.982. The linked-instance workload and both memory
comparisons remain worse than CPython.

Traces show zero reconstruction exits in either modified probe. The preceding
linked-instance runtime exits 486 times during 1,000,000 iterations. This
removes repeated cleanup overhead as well as duplicate owning references.

The three-cycle full suite gives these geometric means. Workload time covers
23 fixtures; process metrics include startup for 24 fixtures.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.000 | 1.002 | 1.048 |
| Process elapsed time | 0.991 | 0.990 | 1.359 |
| Process CPU | 0.992 | 0.992 | 1.349 |
| Peak RSS | 0.992 | 1.001 | 1.702 |

Seven-cycle rechecks confirm the standard attribute-access improvement:
workload time is 0.905 and peak RSS 0.927 times the preceding runtime. The
initial DeltaBlue, nested-loop, and JIT-kernel slowdowns do not repeat; their
JIT workload ratios are 0.994, 1.003, and 0.996, respectively. Interpreter-only
call overhead remains 2.0% slower, and generator JIT peak RSS is 2.1% higher.
A separate seven-cycle pi-digits recheck gives workload ratios of 1.007
(default JIT) and 1.003 (interpreter-only). The initial list-operations RSS improvement does not repeat (1.008), so it
is not a retained memory claim.

## Validation

Two Rust unit tests check stale indices across different activation tables,
range checks, exact instance/string/bytes identity, and reference ownership.
The Python regression covers replaced links, rotating links, recursive calls,
interleaved generators, slot-backed attributes, class mutation, equal but
distinct string and bytes values, and weak references after retirement.

All 28 selected regression scripts pass in default-JIT, interpreter-only, and
free-threaded modes (84 runs), including existing pin-pressure, retirement,
generator, tracing, weakref, GC, and shared-code tests. The new regression
also passes on CPython. Traces confirm compiled execution of its chain,
rotation, recursion, generator, string, and bytes functions. The release CLI
build, formatting, and targeted VM Clippy pass, with the same two preexisting
local Clippy exclusions documented in preceding reports.

```sh
cargo build --release -p weavepy-cli --bin weavepy
WEAVEPY_STDLIB_CACHE="$PWD/target/performance/stdlib-cache" \
python3.14 tools/bench_compare.py \
  --base /path/to/c43b59d/weavepy --new target/release/weavepy \
  --probe tools/bench_attribute_chains.py --work 1000000 --warm --samples 7 \
  --frozen-cache-root target/performance/fresh-pin-caches \
  --out target/performance/attribute-pins.json
```

Repeat with `tools/bench_attribute_text.py` and fresh cache/output paths for
the string/bytes probe. Raw results remain under `target/performance/pin-reuse-*`.
Standard fixtures, work sizes, CI baselines, and gate thresholds are unchanged.
