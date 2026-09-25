# Cache native subscript resolution

After interpreter Fast-subscript admission, the indexed-read profile still
spends about one-fifth of active samples resolving the class method and native
registration. Each instruction now caches the typed static fast function,
guarded by the process-unique class attribute version and native-registration
generation. The cache holds no class, builtin, or container owner. Whole-body
native leaves keep their ordinary resolution path.

The helper reads an existing method table before allocating a slot for a
resolved fast body. Python-defined methods don't pay the temporary-owner probe.
Every native read still rejects potentially last-reference containers before
doing work. Class mutation, inherited mutation, registration changes, unusual
attribute access, exotic keys, callbacks, and shared storage retain their
existing guards and fallback behavior. JIT admission and deque storage are
unchanged.

## Measurements

The baseline is `5896708`, the preceding native-subscript admission increment.
Measurements use macOS x86-64 and optimized CPython 3.14.5, with the same paired
process-order, warmup, frozen-cache, and exclusive-timing method documented in
[Native subscripts in interpreter dispatch](PERFORMANCE-NATIVE-INTERPRETER-SUBSCRIPTS.md).
The following ratios are candidate/baseline; lower is better. Seven paired
cycles retain unchanged work sizes and timed setup/result checks.

| Workload | JIT work | Interpreter work | JIT elapsed | JIT CPU | JIT RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Complete deque fixture | 0.983 | 0.985 | 0.985 | 0.983 | 1.002 |
| Native indexed reads | 0.675 | 0.683 | 0.806 | 0.801 | 1.006 |
| Queue append/pop | 1.011 | 0.999 | 1.004 | 1.005 | 1.000 |
| Stack append/pop | 1.001 | 0.991 | 1.000 | 0.998 | 1.005 |
| Python-defined reads | 0.991 | 1.012 | 0.990 | 0.995 | 0.999 |

Native indexed reads improve another 32%, and the complete deque fixture
another 1.5-1.7%. Default-JIT workload time is still 3.912 times CPython for
indexed reads and 3.331 times for the complete deque. Python-defined reads
retain a large gap, about 16.23 times CPython in this probe.

Three cycles of the unchanged 24-fixture suite give default-JIT geometric means
of 1.001 workload time, 0.995 process elapsed, 0.995 CPU, and 0.996 peak RSS.
Interpreter-only means are 0.997, 0.986, 0.985, and 0.997. Workload means cover
23 fixtures and exclude startup. Default-JIT means against CPython are 1.063,
1.346, 1.265, and 1.566, respectively. The broad suite independently repeats
the deque result at 0.985 JIT and 0.986 interpreter workload time.

The initial broad run records nbody JIT at 1.059, Fibonacci JIT at 1.073,
interpreter JSON at 1.064, and list operations at 1.020 JIT/1.025 interpreter.
Seven-cycle rechecks at the original work sizes give 1.009 for nbody JIT,
1.014/1.023 for Fibonacci JIT/interpreter, 0.996/0.997 for JSON, and
1.007/1.009 for list operations. List JIT RSS remains 1.018 on repeat, and
interpreter JSON RSS remains 1.023, down from 1.061 initially. These smaller
costs remain; rechecks don't replace the original suite results or means.

Thirty-one startup cycles give default-JIT elapsed ratios of 0.993 ordinary,
0.996 no-site, 0.985 isolated, and 0.993 imports. The initial no-site CPU ratio
of 1.044 prompted another 31-cycle startup run; it repeats at 1.001 CPU and
0.988 elapsed. Other repeated startup RSS ratios stay within about 0.5% of
baseline. Ordinary startup and imports still retain substantial CPython gaps.

## Validation and provenance

All 384 VM tests, the unchanged 1 MiB embedding test, VM Clippy with its two
existing exclusions, no-default compilation, and scoped formatting pass. JIT
sources and benchmark tools are unchanged from their preceding complete test
passes. The isolated VM test counts thousands of actual cache hits and verifies
exact-once fast/full completion. It changes registration without changing the
builtin or class identity, then verifies that the newly declined error reaches
the full body. Replaced class and builtin weak references become dead while
the reader code and its caches remain alive. The cache entry measures 32 bytes
on this x86-64 build.

The frozen release passes 267 regression runs in JIT, interpreter-only, and
GIL-disabled modes, 64 container probes, and both CPython native-container
fixtures. The expanded inherited-method fixture also passes in all three
modes on the preceding binary. Ten complete deque results match CPython in
each mode. Temporary-container finalization, callback frames, descriptors,
index conversion, returned-object identity, and shared storage remain covered.

The release binary remains 51,868,080 bytes. Its SHA-256 is
`bbac0c1a83367f561411323c45d30e655579c51330f4f01bcdd70216b09f034c`;
the baseline is
`f0f038e05de194477b4f68d70dbefaa90a8ba2eeb8c7f8d75f0e32ce2467843c`.
Raw paired results, immutable binaries, source hashes, the full patch, and
validation logs remain under `target/performance/native-subscript-cache-*`.
The CLI release build completed and closed before the binary was copied.
