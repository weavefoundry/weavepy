# Borrowed scalar calls with shared native resolution

A dynamic call previously cloned the function and code handles, resolved an owning
native callee, entered the compiled body, and then released those owners on every
invocation. For certified scalar leaf functions, that ownership traffic dominated the
small amount of arithmetic. The optimized path borrows the existing function pin and
thread-local compilation artifact for the call. It requires exact arity, bounded local
and operand buffers, no global/callee/math guards, scalar argument tags, and the
engine's explicit scalar-leaf certificate. The existing GIL checkpoint, live-code
identity check, observer/pending-work gates, recursion limit, and stack-growth checks
remain.

The same cache lookup also serves functions that need an owning callee. It returns that
resolution to the existing native-call machinery for namespace guards, defaults, or
framed execution. Every cache borrow ends before those operations can run Python.
Missing and ineligible functions proceed directly to interpreter fallback. Result
adaptation uses the existing native call protocol. JIT analysis, lowering, admission,
budgets, and pin limits are unchanged.

A preceding isolated borrowed-call candidate improved scalar callbacks by 33-35%, but
guarded-global and default-argument callbacks regressed about 22% because resolution was
repeated after the shortcut declined. That source is held. A separate wider-comparison
experiment is also held: newly compiled frames mishandled a callback's write through the
caller's frame.f_locals. The comparison regression committed as 54cc762 prevents
accepting that behavior; this change preserves the accepted comparison admission.

## Measurements

The baseline is runtime commit `43fd6b3`; `54cc762` adds regression coverage without
changing it. Measurements use macOS x86-64 and optimized CPython 3.14.5, with
alternating paired process order, warmup, unchanged work sizes, frozen caches, and no
overlapping builds, profiles, or timing controllers. Ratios below are
candidate/baseline; lower is better. Setup and result checks stay timed.

| Scalar callback | JIT work, 200,000 calls | JIT elapsed | JIT CPU | JIT RSS | JIT work, 1,000,000 calls |
| --- | ---: | ---: | ---: | ---: | ---: |
| One argument | 0.651 | 0.860 | 0.852 | 1.002 | 0.668 |
| Two arguments | 0.640 | 0.848 | 0.850 | 1.001 | 0.647 |
| Constant return | 0.991 | 0.971 | 0.991 | 1.009 | 1.025 |
| Guarded global | 0.838 | 0.917 | 0.909 | 1.001 | 0.849 |
| Trailing default | 0.850 | 0.938 | 0.937 | 1.001 | 0.827 |
| Branch | 0.999 | 0.996 | 1.003 | 1.006 | 1.012 |

The standard probes use seven paired cycles; the larger probes use five. One- and two-
argument callback work remains 1.130/1.199 times CPython at 200,000 calls and
1.014/1.065 times at one million calls. Larger-probe process elapsed improves about 26%,
and CPU about 27-28%, for these two cases. Separate traces confirm identical compilation
and native-call counts: each simple/global/default probe makes 199,950 scalar native
calls with no native-call deopts. The unused-parameter and branch probes retain their
existing fallback behavior.

Three cycles of the unchanged 24-fixture suite give default-JIT geometric means of 0.994
workload time, 0.979 process elapsed, 0.987 CPU, and 1.001 peak RSS. Interpreter-only
means are 0.996, 0.984, 0.983, and 0.995. Workload means exclude startup. Against
CPython, default-JIT means are 1.059, 1.354, 1.270, and 1.567, respectively. This
remains far from overall CPython parity, particularly for application workloads,
startup, and memory.

The initial suite records DeltaBlue/list JIT RSS at 1.082/1.092, spectral norm JIT work
at 1.047, dictionary interpreter work at 1.058, string interpreter work at 1.032, and
JIT kernels at 1.032. Seven-cycle rechecks give DeltaBlue/list JIT RSS of 0.973/0.946,
spectral norm JIT work of 1.029, dictionary JIT/interpreter work of 1.039/1.045, string
JIT/interpreter work of 1.014/1.029, and JIT kernels of 0.990. Richards JIT work is
1.013 and RSS 1.019 on repeat. The million-call constant-return probe repeats at 1.021
work and 1.006 RSS. These smaller costs remain open; the original measurements and
geometric means are retained.

Thirty-one startup cycles give default-JIT elapsed ratios of 0.994 ordinary, 1.009 no-
site, 1.012 isolated, and 0.997 imports. No-site CPU is initially 1.040 and repeats at
0.989 in a separate complete 31-cycle startup run. Repeated elapsed ratios are 0.995
ordinary, 1.009 no-site, 0.991 isolated, and 0.987 imports. Ordinary/isolated CPU remain
1.019/1.012, and no-site RSS remains 1.010.

## Validation

The focused VM test executes both borrowed calls and reused owning resolutions more than
1,000 times. It covers arithmetic and overflow, booleans and floats, subclasses,
exception frames, observers, code/default replacement, recursion, distinct namespaces
sharing code, non-leaf branches, and unused parameters. The 385-test VM suite, 71-test
JIT suite, unchanged 1 MiB embedding test, strict JIT lint, VM lint with its existing
two exclusions, formatting, no-default-feature compilation, and 14 benchmark-tool tests
pass. The frozen release passes 276 regression runs across 92 fixtures in JIT,
interpreter-only, and GIL-disabled modes, plus 256 probe checks. This includes three
supplemental runs of the expanded scalar-leaf fixture and three of the new namespace-
guard fixture. The namespace equality callback starts another JIT compilation, checking
that the owning fallback releases the cache borrow before Python runs; a separate trace
confirms that nested compilation. This fixture also passes CPython and all three modes
on the accepted baseline. Full deque results, operand order, writable caller locals, and
temporary-owner behavior retain their existing checks.

The release binary is 51,872,928 bytes, 4,848 bytes larger than baseline. Its SHA-256 is
`553f095b76a8eae3f0074aab14aa209b09b762fca67f4a76f93ee036afffa187`; the baseline is
`bbac0c1a83367f561411323c45d30e655579c51330f4f01bcdd70216b09f034c`. Sources, manifests,
immutable binaries, raw results, held experiments, and logs remain under
`target/performance/`. The `resolved-dynamic-scalar-calls-*` files describe this
increment. The release build completed and its controller closed before the binary was
copied.

## Reproduction

The reusable probe checks one- and two-argument scalar callees, a constant return, a
guarded global, a trailing default, and a branch. Its setup and result check stay timed;
the call-kind environment setting is recorded in the comparison JSON. For example:

```sh
WEAVEPY_SCALAR_CALL_KIND=one python3.14 -B tools/bench_compare.py \
  --base target/performance/weavepy-before \
  --new target/release/weavepy \
  --probe tools/bench_scalar_calls.py --work 200000 --samples 7 \
  --frozen-cache-root target/performance/scalar-call-caches \
  --out target/performance/scalar-call-comparison.json
```

Repeat with `two`, `constant`, `global`, `default`, and `branch`. Use otherwise idle hardware. The broad suite and startup cases must retain their existing work sizes; larger scalar-call probes are separate measurements. Original negative results remain alongside repeats and later candidates.
