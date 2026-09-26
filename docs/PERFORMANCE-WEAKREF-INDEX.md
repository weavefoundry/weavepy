# Hash weak-reference identities and preserve collection order

The weak-reference registry now uses a hash index for identity lookups. It shares the collector's existing address mixer, which distributes aligned allocation addresses across buckets. Each referent's watchers remain in registration order, and clearing still visits them newest first. Selected snapshots sort ids before acquiring target owners; excluded targets aren't upgraded, borrowed, or cloned. Exact strong-clone accounting and locking remain unchanged. Registry maintenance releases excess table capacity and rebuilds its miss filter without a temporary id vector.

The comparison uses `076e0aa`, optimized CPython 3.14.5, and identical release settings on Intel macOS. Ratios are candidate/base; lower is better. There are no changes to object layouts, collection scheduling, JIT admission, workloads, benchmark gates, or baselines. Setup, result checks, and normal collection remain timed.

Seven paired cycles cover five GC workloads at two population sizes:

| Population | 10,000 objects, JIT/interpreter | 100,000 objects, JIT/interpreter |
|---|---:|---:|
| Cycles | 0.952 / 0.949 | 0.944 / 0.956 |
| Finalizers | 0.954 / 0.923 | 0.969 / 0.957 |
| Weak references | 0.957 / 0.949 | 0.885 / 0.910 |
| Callbacks | 0.956 / 0.941 | 0.905 / 0.890 |
| Frozen graph | 0.962 / 0.959 | 0.946 / 0.956 |

An earlier five-cycle selection also improved the three measured 100,000-object cases. Memory results are mixed. The larger weak-reference case has JIT RSS ratios of 0.972 in selection and 1.030 in the expanded run. The 10,000-object callback case initially uses 5.9% more JIT RSS; an eleven-cycle repeat gives 1.007, with work ratios of 0.972/0.966. No general memory reduction is claimed. The larger populations still take about 18 to 37 times CPython's workload time and roughly 6.6 to 7.5 times its peak RSS with JIT enabled.

Seven-cycle access measurements give JIT/interpreter work ratios of 0.947/0.974 for ordinary refs, 0.941/0.943 for subclasses, 0.973/0.957 for proxies, 0.971/0.972 for weak-key dictionaries, and 0.943/0.943 for weak-value dictionaries. Callable proxies remain near the base at 1.002/1.010.

The unchanged 24-fixture suite, measured over three paired cycles, has these geometric means. Work time excludes the startup-only fixture.

| Engine | Work | Process elapsed | Process CPU | Peak RSS |
|---|---:|---:|---:|---:|
| JIT/base | 1.0033 | 1.0009 | 1.0036 | 0.9991 |
| Interpreter/base | 0.9995 | 0.9975 | 0.9936 | 0.9994 |
| JIT/CPython | 1.0148 | 1.3905 | 1.2527 | 1.5534 |

Eleven-cycle repeats cover fourteen applications. The initial larger JIT Fibonacci, float, and attribute-access costs don't persist. Richards work remains 2.6%/2.0% slower, with JIT RSS 2.2% higher. Deque work is 2.7%/1.7% slower, with CPU 2.5%/1.4% higher. Interpreter nested loops cost 4.9% in work and 2.8% in CPU; interpreter float work costs 2.4%, and DeltaBlue CPU costs 2.4%. Repeated JIT jitloop work is 2.5% higher while process elapsed and CPU are within 0.2%; interpreter sumvm work costs 2.8% while process metrics remain near the base. Dictionary work improves 3.3%/4.2%, but its JIT process metrics don't improve; interpreter elapsed/CPU improve 3.0%/2.7%. These costs are retained alongside the GC gains.

Both startup runs use 31 paired cycles for four cases. Repeated JIT elapsed ratios for normal, no-site, isolated, and import startup are 1.013, 1.010, 1.009, and 1.008. CPU ratios are 1.024, 1.030, 1.024, and 1.006; RSS is within 0.7% of the base. Interpreter elapsed ratios are 0.995, 0.983, 0.999, and 1.003. Normal JIT startup still takes 1.673 times CPython's elapsed time.

All 403 VM tests pass, including new cases for dead watchers, exact target counts, callback order, sorted selected snapshots, excluded borrowed payloads, and registry reuse after pruning. Embedding's explicit 1 MiB stack case, VM Clippy with established exclusions, no-default compilation, formatting, the artifact policy, and fourteen benchmark-tool tests pass. The frozen CLI passes 321 runs across 107 regression fixtures, 1,048 semantic probes, twenty CPython fixtures, 180 ordinary-population checks, 100 GC-population checks, and 120 checks each for weak-reference access and saved calls. All 53 probe, three application, and nine population compilation decisions are unchanged.

All 27 unmodified upstream selections pass without ignored exceptions: selected call/descriptor cases, `test_gc`, `test_weakref`, and `test_weakset` in JIT, interpreter, and GIL-disabled modes. Each mode passes 137 weak-reference tests with seven expected skips, 57 GC tests with twelve skips, and 46 weak-set tests. A five-second diagnostic profile during JIT validation still points mainly to collector traversal and borrow bookkeeping; all validation and profile timings are excluded. This clean run does not establish that the previously reproduced concurrent shared-function mutation issue is fixed.

Performance timing ran without overlapping owned builds, tests, or profiles. Process-name snapshots record background desktop activity; this isn't a dedicated idle host. Initial and repeated results are preserved. The release binary is 51,872,320 bytes, 9,248 fewer than the base, with SHA-256 `60ca4a85ce04b98e36516744ba9967f7821fdcb6c729e263361a629081101c81`. The runtime patch hash is `378277ac1a02c777edb4e53388bace05951ceaa4c5650fa5880542528dd63b41`. Raw measurements, logs, and source snapshots are under `target/performance/weakref-index-investigation/`; the full-suite and initial startup results use the sibling `weakref-hash-index-r076e0aa-` prefix.

The previous commit's Windows benchmark gate still fails sumvm at 1.246 times merge-base and nested loops at 1.161 after retries. Its warmed execution is near merge-base; cold compilation remains a separate problem. This optimization doesn't resolve that gate or establish CPython performance parity. The native weak-reference storage experiments remain parked.
