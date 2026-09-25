# Native subscripts in interpreter dispatch

Registered native `__getitem__` implementations can declare a pure fast half
that declines arguments requiring Python callbacks. Interpreter subscripts
previously admitted only whole-body native leaves, so deque indexing took the
full call protocol even for exact integer indices. The new helper offers the
two borrowed operands to the registered fast half. A decline leaves the
operands untouched for the ordinary handler; a completed result or error is
consumed exactly once.

The helper retains the instance-binding, no-keyword, ordinary-attribute, and
exotic-key checks. It rejects potentially last-reference containers before
reading them. An earlier unguarded prototype delayed finalization of contents
in temporary deques; it was rejected before timing. The regression now checks
finalizer effects after each temporary is consumed. Index conversion callbacks
and shared-storage behavior still use their existing guarded paths. JIT
admission and lowering are unchanged.

## Method and results

Measurements use macOS x86-64 and CPython 3.14.5 with PGO, LTO, and the tail-call
interpreter, with its experimental JIT disabled. The preceding accepted runtime
is `cb9d647`; subsequent commits through `8bc590a` add tests and tools only.
Paired process order alternates, the warmup cycle is discarded, and separate
frozen caches remain unchanged. Builds, tests, profiles, and other benchmarks
don't overlap timing. Setup and result checks remain timed. All ratios below
are candidate/baseline, so lower is better.

Seven paired cycles at unchanged work sizes give:

| Workload | JIT work | Interpreter work | JIT process elapsed | JIT CPU | JIT peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Complete deque fixture | 0.926 | 0.916 | 0.932 | 0.933 | 0.994 |
| Native indexed reads | 0.421 | 0.406 | 0.563 | 0.544 | 0.997 |
| Queue append/pop | 0.972 | 0.979 | 0.961 | 0.947 | 1.003 |
| Stack append/pop | 0.974 | 0.976 | 0.982 | 0.984 | 0.999 |
| Python-defined indexed reads | 1.009 | 1.017 | 1.009 | 1.014 | 1.004 |

The native read probe improves by 58-59%, and the complete deque fixture by
7-8%. Against CPython, default-JIT work is still 5.923 times as long for the
read probe and 3.391 times as long for the complete deque. These gaps remain.
The queue and stack changes are controls, not claimed direct benefits of the
new subscript path.

Three cycles of the unchanged 24-fixture suite yield geometric means of
0.998 workload time, 0.983 process elapsed, 0.987 CPU, and 0.990 peak RSS for
default JIT. Interpreter-only means are 0.992, 0.980, 0.982, and 0.993.
Workload means cover 23 fixtures and exclude startup. Default-JIT means against
CPython remain 1.072 workload time, 1.363 elapsed, 1.280 CPU, and 1.566 RSS.
The complete deque fixture independently repeats at 0.927/0.919 workload time
in JIT/interpreter modes.

The initial broad run also records costs: sumvm JIT 1.073, string methods
1.050, pidigits 1.031, pyaes 1.027, nbody 1.026, and interpreter list operations
1.041. Seven-cycle rechecks at the original work sizes give sumvm 0.992,
string methods 1.016, pidigits 1.013, pyaes 1.023, nbody 1.033, and interpreter
list operations 0.992. The Python-read control repeats at 1.041 JIT and 1.015
interpreter workload time. These smaller costs remain visible; the repeat
doesn't replace the original full-suite results or means. In particular, nbody
and pyaes still need attention despite the much larger native-read gain.

Thirty-one startup cycles give default-JIT elapsed ratios of 0.995 for ordinary
startup, 0.995 without site, 0.989 in isolated mode, and 0.986 for imports.
The corresponding CPU ratios are 1.001, 0.995, 1.005, and 0.988; RSS ratios are
1.000, 1.005, 1.000, and 0.997. Ordinary startup still takes 1.502 times
CPython's elapsed time, and imports 2.483 times.

## Validation and provenance

The candidate passes all 384 VM tests and the unchanged 1 MiB embedding test,
VM Clippy with its two existing exclusions, no-default-feature compilation,
and scoped formatting. The unchanged JIT sources and benchmark tools retain
the preceding complete 71-test JIT and tool-test passes. The isolated VM test
counts native and full executions to verify thousands of fast reads, exact-once
completion, boxed values, errors, declined arguments, and method replacement.
Its full and fast test bodies implement equivalent behavior.

The frozen release passes 89 regression scripts in JIT, interpreter-only, and
GIL-disabled modes, for 267 runs, plus 64 native-container probe checks. Both
native-container fixtures pass on CPython. The complete deque workload returns
identical results to CPython at ten sizes in all three execution modes. Coverage
includes callbacks and caller locals, descriptors, truth conversion, index
conversion, returned-object identity, weak references, shared storage, and
prompt temporary finalization.

The release binary remains 51,868,080 bytes, equal to the preceding runtime.
Its SHA-256 is
`f0f038e05de194477b4f68d70dbefaa90a8ba2eeb8c7f8d75f0e32ce2467843c`.
The baseline binary's SHA-256 is
`440c2baa1440dda31a05bea788e5b1cc766cf825f8e13ff3bc14e3d0f6302e3f`.
Source hashes, complete patches, immutable executables, validation logs, and
raw paired measurements remain under
`target/performance/interpreter-native-subscript-guarded-investigation/` and
`target/performance/interpreter-native-subscript-guarded-*.json`. The binary was
copied only after the successful CLI release build completed and closed.

Separate JIT native-method and indexed-read prototypes remain excluded because
they worsened the full deque workload or memory use. This change doesn't claim
to solve their admission problems, the Windows cold-compilation regression, or
the preexisting bounded-deque eviction finalizer timing and caller-frame gap.
