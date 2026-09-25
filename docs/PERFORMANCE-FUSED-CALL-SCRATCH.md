# Initialize fused-call scalar scratch on demand

Simple global, static, and bound calls staged small-integer arguments in an
eight-element `Object` array initialized to `None`. Release assembly initialized
128 bytes even when no argument needed scratch, and each integer assignment
called general `Object` drop glue before replacing `None`.

The reader now uses `MaybeUninit` slots and initializes only the integers it
needs, before publishing their pointers. Every initialized scratch value is an
unboxed integer; unused slots are never read, and no slot owns a heap value that
needs cleanup. The argument-pointer array remains initialized. Supported call
shapes, ownership, defaults, recursion, observers, JIT admission, and warmup
checks are unchanged. The held field-argument and escaped-local changes are
absent.

On the measured x86-64 build, the global handler's initial SIMD zero stores fall
from twelve to four. The integer assignment no longer calls drop glue. Each
handler retains the necessary cleanup for an owned overridden-defaults value.
The executable shrinks by 32 bytes to 51,877,168 bytes; this doesn't establish a
process-memory improvement.

## Measurement

These measurements use macOS x86-64, an Intel Core i9-9980HK, Rust 1.94.0,
standard release settings, and optimized CPython 3.14.5. The accepted baseline
is `6ea3e53`. Batches run after builds, tests, traces, and profiling finish, using
alternating paired process order, discarded warmup, and verified frozen caches.
All original samples remain available. Ratios below are modified/baseline;
lower is better.

The literal probes time complete callers, including receiver construction and
result checks. Standard work uses 200,000 calls and seven cycles; sustained
work uses one million calls and five cycles.

| Caller | Standard JIT | Standard interpreter | Sustained JIT | Sustained interpreter |
| --- | ---: | ---: | ---: | ---: |
| Global, one literal | 0.987 | 0.979 | 0.994 | 0.980 |
| Static, one literal | 1.002 | 0.972 | 1.011 | 0.965 |
| Bound method, one literal | 0.922 | 0.919 | 0.937 | 0.924 |
| Eight arguments, seven literals | 0.989 | 0.792 | 0.990 | 0.794 |
| Zero arguments | 1.012 | 0.966 | 0.980 | 1.015 |

The strongest repeated gains are bound literal calls and eight-argument
interpreter calls. Their sustained JIT workload times are still 1.145 and 0.892
times CPython, respectively. Sustained JIT RSS ratios for all five literal
callers range from 1.001 to 1.006.

The unchanged 24-fixture suite uses three cycles. Workload means cover 23
fixtures and exclude startup; process metrics cover all 24.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.990 | 0.993 | 1.062 |
| Process elapsed | 0.973 | 0.986 | 1.382 |
| Process CPU | 0.978 | 0.984 | 1.265 |
| Peak RSS | 0.996 | 0.998 | 1.548 |

These means don't establish an application-wide speedup or superiority to
CPython. Seven-cycle rechecks reduce the initial cold-JIT-loop ratio from 1.059
to 1.005, and its warmed ratio is 0.997. AES's initial 0.926/1.036 JIT/interpreter
ratios become 0.994/0.996. Fannkuch's initial 0.960/0.936 becomes 0.977/0.988.
JSON's initial 1.026 JIT cost becomes 0.987. None of the original observations
is discarded.

The complete call-overhead fixture initially measures 1.014/0.960, then
0.990/1.002 over seven more cycles. At five times the standard work, seven cycles
measure 0.991/0.997. An application gain isn't established. The hook fallback's
initial 1.035 JIT cost falls to 1.022 at sustained work and 1.002 in a seven-cycle
sustained recheck; interpreter ratios are 1.005/1.006/1.008. The two-argument
scalar control's initial 1.024 interpreter cost doesn't repeat at sustained
work (1.001).

Thirty-one startup cycles measure JIT elapsed ratios of
0.993/0.989/0.994/0.993 for normal/no-site/isolated/import cases. CPU ratios are
1.000/1.004/1.003/0.997 and RSS ratios are 1.004/1.003/1.004/1.000. Interpreter
elapsed ratios range from 0.956 to 0.980. Normal startup and imports still take
1.603 and 2.559 times CPython's process time with JIT.

Thirty-one-cycle memory checks reduce pi-digits' initial 8.3% RSS increase to
0.5%; DeltaBlue's initial 8.8% reduction becomes 0.2%, and list operations'
initial 10.3% reduction becomes a 0.1% increase. These large memory changes
aren't confirmed. DeltaBlue work is 0.995/0.998 and list work is 0.992/0.990 in
those rechecks. Richards' earlier 1.6% RSS increase becomes 0.1% over 31 cycles,
with work ratios of 0.998/1.010.

This increment is retained for the repeated literal-call gains and removal of
unnecessary work, with the complete controls and rechecks above. A broad
application or memory improvement isn't established. WeavePy still hasn't
achieved the objective of outperforming CPython across every meaningful metric.

## Validation and provenance

The behavior fixture passes CPython and the accepted runtime in all three
execution modes before the change. It covers every literal position, bound
receiver offsets, argument limits, mixed constants/locals, defaults, code and
binding changes, zero arguments, later-operand fallback, exceptions, callback
frames, returned values, and tracing. Completed-call counters retain 3,998 hits
per 4,000-call probe with JIT disabled. With JIT enabled, the bound probe records
2,953 and the others 689; native calls may bypass interpreter counters.

All 389 VM tests, embedding, Clippy with the established exclusions, no-default
compilation, scoped formatting, and 14 benchmark-tool tests pass. The frozen
release passes 294 runs across 98 fixtures, 848 semantic probes, eleven CPython
fixtures, and additional result/frame/ownership comparisons. All 43 checked
compilation decisions and JIT statistics match the accepted baseline.

The release build takes 7 minutes, 34 seconds; its controller closes before
freezing. The binary SHA-256 is
`66eac78bac5d29019b3439450855aaa1ca4c13f20b07d5b750cb78f1685ef78d`.
The build's source patch, including the new fixture and probe, is
`f9a1c5db44e89ba6c53d8e4110d58b9db368a3619cff14436a881c41da01f1b7`
against `01e6b16`. Snapshots, assembly, raw samples, traces, profiles, and the
immutable executable remain under `target/performance/`.
