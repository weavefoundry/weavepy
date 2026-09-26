# Remove quadratic entry shifts from set deletion

Sets now replace a removed entry with the final entry instead of shifting the remaining entries. This applies to Python discard, remove, and difference-update operations and C API discard. Python and C API pop take the final stored entry directly, preserving its owner without calling its hash or equality methods. Set iteration has no insertion-order contract; dictionaries retain their existing ordering behavior. Deferred mutation replay, removed-value handling, and callback-safe difference-update snapshots remain intact.

The comparison uses accepted runtime `8411107`, optimized CPython 3.14.5, and identical release settings on Intel macOS. Setup, assertions, and normal collection remain timed. Ratios are candidate/base; lower is better. No object layout, collection schedule, JIT admission, workload, baseline, or gate changes are included.

A five-cycle selection at 10,000 elements yields JIT/interpreter work ratios of 0.054/0.057 for pop, 0.056/0.048 for remove, 0.028/0.031 for discard, 0.028/0.031 for difference-update, and 0.058/0.060 for remove/add churn. Weak-set cleanup is 1.026/1.000, so that selection does not show a cleanup gain. These results are retained separately from the expanded matrix.

Seven paired cycles at each size give these workload ratios:

| Operation | 1,000, JIT/interpreter | 10,000, JIT/interpreter | 100,000, JIT/interpreter |
|---|---:|---:|---:|
| Pop | 0.4148 / 0.4419 | 0.0557 / 0.0549 | 0.0055 / 0.0054 |
| Remove | 0.4162 / 0.3829 | 0.0558 / 0.0554 | 0.0048 / 0.0047 |
| Discard | 0.3008 / 0.2404 | 0.0302 / 0.0279 | 0.0025 / 0.0023 |
| Difference-update | 0.3087 / 0.2947 | 0.0294 / 0.0290 | 0.0030 / 0.0030 |
| Remove/add churn | 0.3906 / 0.3742 | 0.0580 / 0.0550 | 0.0063 / 0.0063 |
| Weak-set cleanup | 0.9954 / 1.0236 | 0.9892 / 0.9685 | 1.0073 / 0.9849 |

For the five ordinary set operations, a tenfold increase from 10,000 to 100,000 elements increases the base workload time about 97-128 times, versus 10-13 times for the candidate. Large difference-update peak RSS ratios are 0.862/0.864. Large churn interpreter RSS costs 2.5%; weak-set JIT RSS costs 2.7%. Other large ordinary-set memory ratios stay within 0.7% of the base. Weak-set cleanup has no established general speedup.

At 100,000 elements, JIT workload/CPython ratios are 5.034 for pop, 5.507 for remove, 2.986 for discard, 1.785 for difference-update, and 3.899 for churn; peak RSS remains 1.045-1.295 times CPython. Weak-set cleanup still takes 41.0 times CPython workload time and 6.5 times its peak RSS.

The unchanged 24-fixture suite uses three paired cycles. Work excludes the startup-only fixture; process metrics include all 24.

| Engine | Work | Process elapsed | Process CPU | Peak RSS |
|---|---:|---:|---:|---:|
| JIT/base | 0.9993 | 0.9908 | 0.9939 | 0.9989 |
| Interpreter/base | 1.0010 | 0.9834 | 0.9893 | 0.9963 |
| JIT/CPython | 1.0111 | 1.3684 | 1.2373 | 1.5549 |

Eleven-cycle repeats cover twenty applications: every initial change exceeding 3% in either direction in any measured metric, plus call, float, and nested-loop controls. The initial interpreter Richards/Fibonacci/AES/jitloop slowdowns, JIT sumvm/list/datetime slowdowns, and list RSS increase do not repeat. Costs remain in the repeat: interpreter sumvm work/elapsed/CPU ratios are 1.074/1.043/1.044, dictionaries are 1.031/1.031/1.027, and n-body work is 1.040 with process elapsed/CPU of 1.013/1.017. JSON JIT RSS is 1.045; Richards JIT work is 1.015 with process CPU near the base. Original sumvm and n-body interpreter work ratios were 0.928 and 0.914, so their movement does not establish a stable general improvement. DeltaBlue repeats at 0.994/1.004 in work and JIT RSS 1.007; the initial 5.9% RSS reduction is not confirmed. Both runs remain recorded.

Both startup sweeps use 31 paired cycles per case. Repeated JIT elapsed ratios for normal, no-site, isolated, and import startup are 1.002/1.005/1.005/1.004; CPU ratios are 1.008/1.014/1.046/1.000, and RSS is within 0.4% of the base. The isolated CPU cost rises from the original 1.022. Repeated interpreter elapsed ratios are 0.980/0.967/0.986/0.990. Normal JIT startup still takes 1.654 times CPython elapsed. No general application or startup improvement is claimed.

All 403 VM tests, the new C API set integration test, embedding's explicit 1 MiB stack case, VM and C API Clippy with established exclusions, and no-default compilation pass. The frozen CLI passes 324 runs across 108 regression fixtures, 1,048 semantic probes, twenty-one CPython fixtures, 180 ordinary-population checks, 100 GC-population checks, and 120 checks each for weak-reference access, saved calls, and set mutations. Fourteen benchmark-tool tests pass. All 53 probe, three application, nine population, and six set compilation decisions are unchanged.

The new Python and C API coverage checks membership, collisions, subclasses, original object identity, missing-key errors, iteration invalidation, finalization, weak-reference lifetime, and pop without hash/equality callbacks. CPython passes the Python fixture; the base passes its earlier checks and fails the new pop callback check. The base calls hash eleven times when popping twelve stored colliding keys; CPython and the candidate call it zero times.

All 27 unmodified upstream selections pass without ignored exceptions in JIT, interpreter, and GIL-disabled modes: selected call/descriptor cases, test_gc, test_set, and test_weakset. Each mode runs 630 set tests, 57 GC tests with twelve expected skips, and 46 weak-set tests. The accepted base also passes all 630 set tests in each mode. This set candidate does not rerun the full test_weakref module; the previous accepted runtime's clean run is documented separately.

The release binary is 51,876,368 bytes, 4,048 more than the base, with SHA-256 `66fa08a0051c2b48be8cc9671634eb99b1c5900b4c1f86b97a3db301c1dc30e2`. The runtime patch hash is `124a39e0c8072d423690bff1f155c5c4bf4c2a86b1c8346e24660b4a6e98b87a`. Raw results, logs, and source snapshots stay under `target/performance/set-removal-investigation/`; the application and initial startup results use the sibling `set-removals-r8411107-` prefix. Validation timings are excluded. Performance runs do not overlap owned builds, tests, or profiles. Process-name snapshots record background desktop activity, including media analysis using about one CPU core during selection and scaling; this is not a dedicated idle host. Small differences need repeats, while the large set gains are consistent with the changed algorithm.

The accepted base's Windows benchmark gate still fails sumvm at 1.195 times merge-base, with a retry of 1.221. Its warmed execution is near merge-base while cold compilation remains slower; this set change does not address that cause. The large set gains justify this increment, with the costs above retained. WeavePy has not achieved CPython parity across all meaningful metrics.
