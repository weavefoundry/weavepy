# Borrow receivers for bound calls without positional arguments

The generic bound-method fallback now borrows its receiver as a one-element argument slice when there are no explicit positional arguments. The bound method owns the receiver throughout the call, including reentry. This removes one temporary vector allocation and receiver retain/release pair. Descriptor redispatch, sentinel methods, dictionary `__missing__` handling, and keyword processing retain their existing behavior. Most direct Python method calls already take a separate path that transfers arguments into a frame; this change doesn't accelerate that path.

The comparison uses the accepted `bc293dc` runtime, optimized CPython 3.14.5, and the same release settings on Intel macOS. Ratios below are candidate/base; lower is better. Setup, validation, and normal collection remain timed. There are no fixture, benchmark gate, JIT admission, or GC scheduling changes.

| Workload | First seven cycles, JIT/interpreter | Eleven-cycle repeat, JIT/interpreter |
|---|---:|---:|
| Weak-reference dereference | 0.962 / 0.945 | 0.982 / 0.961 |
| Weak-reference subclass dereference | 0.964 / 0.964 | 0.954 / 0.935 |
| Weak-key dictionary lookup | 1.021 / 1.027 | 1.024 / 1.018 |

The repeated subclass workload also reduces whole-process elapsed time by 2.1%/2.4% and CPU by 1.4%/3.1%. Repeated ordinary dereferencing reduces interpreter elapsed time by 2.1% and CPU by 2.6%; its JIT process metrics are effectively unchanged. Peak RSS is within 0.4% of the base in these repeated access cases. The weak-key cost remains visible: repeated process CPU is 1.8%/1.6% higher.

The unchanged 24-fixture suite, measured over three paired cycles, has these geometric means. Work time excludes the startup-only fixture.

| Engine | Work | Process elapsed | Process CPU | Peak RSS |
|---|---:|---:|---:|---:|
| JIT/base | 1.0014 | 0.9985 | 1.0032 | 0.9987 |
| Interpreter/base | 0.9998 | 0.9988 | 0.9985 | 0.9972 |
| JIT/CPython | 1.0228 | 1.3903 | 1.2546 | 1.5977 |

Eleven-cycle repeats cover nine applications. The first-pass Fannkuch, N-body, pidigits, sumvm, list-work, call-work, and generator increases don't persist at their original size. Dictionary work remains 1.5%/3.6% slower, with process CPU 2.2%/2.5% higher. Repeated JIT DeltaBlue and list RSS are 2.1% higher, although their first-pass RSS ratios were 0.989 and 0.982. No broad application speedup is claimed.

The new `tools/bench_bound_calls.py` measures six saved-call forms. Seven-cycle measurements and eleven-cycle repeats show no consistent improvement across ordinary Python, keyword, class, builtin, iterator, and generator calls. It records setup and result checks inside the timer; `bench_compare.py` records its selected environment variable.

Both startup runs use 31 paired cycles for four cases. The repeat's JIT elapsed ratios are 1.016, 1.010, 1.014, and 1.008 for normal, no-site, isolated, and import startup. Their CPU ratios are 1.012, 0.989, 1.012, and 1.006. No-site JIT RSS remains 1.8% higher. Normal startup is still 1.686 times CPython's elapsed time. The initial no-site CPU increase of 10.0% does not repeat.

All 401 VM tests pass, together with embedding's explicit 1 MiB stack case, VM Clippy with established exclusions, no-default compilation, formatting, and fourteen benchmark-tool tests. The frozen CLI passes 324 runs across 107 regression fixtures, 1,048 semantic probes, twenty CPython fixtures, 180 ordinary-population checks, 100 GC-population checks, and 120 checks each for the weak-reference and saved-call probes. The final lifetime fixture also uses expanded arguments to exercise the generic fallback directly; that fixture-only extension passes CPython and all three base/candidate modes. Its additional source hashes and results are recorded separately from the original build manifest. All 53 probe, three application, and nine population compilation decisions are unchanged.

All 27 unmodified upstream runs pass without ignored exceptions: selected call/descriptor cases, `test_gc`, `test_weakref`, and `test_weakset` in JIT, interpreter, and GIL-disabled modes. The weak-reference suite runs 137 tests with seven expected skips per mode. A five-second diagnostic profile was taken during interpreter validation; all validation and profile timings are excluded. This clean run doesn't establish that the previously reproduced concurrent shared-function mutation issue is fixed.

Owned builds, tests, and profiles finished before timing. Process-name snapshots record background desktop activity; this isn't an idle dedicated benchmark host. Small CI-log downloads overlapped the last first-pass access cases; the repeated access measurements ran afterward. Both first-pass and repeated results are retained.

The release binary is 51,881,568 bytes, unchanged from the base, with SHA-256 `4127ad8ad8d03d68d98ed01b9f08f7dcf17d126947b71789b0bfd30321e9a210`. The source patch hash is `376662f6d6d4f5809353943d83f3014367b835524a931fb48d2b2278325ebc95`. Raw samples, validation, profiles, and source snapshots are under `target/performance/bound-zero-argument-investigation/`; full-suite and initial startup results use the sibling `bound-zero-arguments-rbc293dc-` prefix.

The accepted tooling head `9fb3198` has 23 passing CI checks and five failures: Windows sumvm/nested-loop performance gates and Linux/macOS Alembic and lxml ecosystem failures. This change doesn't address those failures or the remaining CPython performance gaps. Both native weak-reference storage experiments remain parked separately.
