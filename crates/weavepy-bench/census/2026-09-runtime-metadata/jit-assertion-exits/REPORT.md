# JIT exits before common exception constants

The overall performance goal remains unachieved.

Candidate ed99cdba treats only canonical AssertionError/NotImplementedError
loads as exact-PC interpreter exits. Successful branches can compile while
message construction, callbacks, and exceptions continue through the existing
interpreter. Other common constants retain their original analysis. Existing
plain-stack checks, spill/writeback, and marker-free call-span exclusion apply.
No new runtime helper, object field, cache, unsafe code, or execution policy.
The binary is 44,289,520 bytes, unchanged from immediate a1e87c1a. Earlier
intern and native-entry changes, including their regressions, remain included.

All 58 JIT tests, 342 VM tests, formatting, Clippy, no-default-feature check,
69 targeted release checks, and 275 standard compatibility checks pass. Six
assertion functions actually compile and rebuild their error continuations
with CPython-equivalent results. The regression includes side effects, message
identity, conditional messages, loop locals, exact traceback line, cleanup,
builtin shadowing, exception context, tracing, recovery, and real bytecode
loading the second common exception constant. Baseline checks are preserved.

The original extra-suite script returned success for a submodule filter that
selected ZERO tests. That isn't a passing assertion suite. Its method and raw
result remain preserved. A separate check verified the original five nonempty
call/inspection/copy suites and ran the discoverable unittest package, requiring
a positive selected count and all modules passing. Timing used only the v2
launcher, which requires this corrected check. No timed sample was replaced.

Untimed datetime values agree with CPython. Compiled functions rise from 24
to 26 (_cmp and _find_isoformat_datetime_separator), while _ymd2ord and field
validators still encounter MixedArithTypes. Compilation counts aren't speed
measurements. A later guarded-comparison draft is separate and unapplied.

Focused timing uses 13 warm and four cold controls, work 5000, and seven
paired samples of both modes of candidate, immediate a1e87c1a, retained 539c,
and CPython. All values and per-binary frozen-cache checks pass. Lower is better.

| Case | Mode | Time / predecessor | RSS / predecessor | Time / CPython |
| --- | --- | ---: | ---: | ---: |
| warm/assert_int | jit | 0.1587 | 1.0022 | 1.6075 |
| warm/assert_int | interp | 0.9777 | 1.0000 | 8.3936 |
| warm/assert_float | jit | 0.1550 | 1.0126 | 1.3007 |
| warm/assert_float | interp | 0.9834 | 1.0030 | 8.3057 |
| warm/assert_list | jit | 0.9621 | 1.0033 | 12.8183 |
| warm/assert_list | interp | 0.9913 | 1.0053 | 11.7376 |
| warm/assert_chain | jit | 0.1479 | 1.0022 | 1.6661 |
| warm/assert_chain | interp | 0.9860 | 1.0024 | 9.2618 |
| warm/failures_none | jit | 0.2875 | 1.0032 | 3.3747 |
| warm/failures_none | interp | 0.9800 | 1.0024 | 9.6615 |
| warm/failures_every_100 | jit | 0.3691 | 1.0016 | 3.9789 |
| warm/failures_every_100 | interp | 0.9979 | 1.0024 | 10.5652 |
| warm/failures_every_2 | jit | 0.9188 | 1.0022 | 24.4227 |
| warm/failures_every_2 | interp | 0.9967 | 0.9994 | 23.7908 |
| warm/failures_every_1 | jit | 0.9767 | 1.0005 | 30.6841 |
| warm/failures_every_1 | interp | 0.9975 | 1.0018 | 28.2990 |
| warm/date_toordinal | jit | 0.9812 | 1.0025 | 27.1458 |
| warm/date_toordinal | interp | 0.9904 | 1.0017 | 24.7062 |
| warm/date_weekday | jit | 0.9932 | 0.9990 | 43.0282 |
| warm/date_weekday | interp | 1.0009 | 1.0022 | 39.5712 |
| warm/datetime_add | jit | 0.9941 | 0.9985 | 435.5173 |
| warm/datetime_add | interp | 0.9944 | 1.0017 | 437.4362 |
| warm/datetime_subtract | jit | 0.9831 | 0.9980 | 75.1045 |
| warm/datetime_subtract | interp | 0.9940 | 1.0022 | 72.8493 |
| warm/failure_burst_recovery | jit | 0.4301 | 1.0005 | 6.4118 |
| warm/failure_burst_recovery | interp | 0.9847 | 1.0024 | 12.3012 |
| cold/failures_none | jit | 0.4109 | 1.0022 | 5.6504 |
| cold/failures_none | interp | 0.9793 | 1.0036 | 9.6348 |
| cold/failures_every_100 | jit | 0.4908 | 0.9989 | 6.2814 |
| cold/failures_every_100 | interp | 0.9849 | 1.0036 | 10.7127 |
| cold/failures_every_2 | jit | 0.9213 | 1.0011 | 25.7415 |
| cold/failures_every_2 | interp | 0.9823 | 1.0024 | 24.3738 |
| cold/failures_every_1 | jit | 0.9738 | 0.9989 | 31.6460 |
| cold/failures_every_1 | interp | 1.0001 | 1.0012 | 28.6311 |

All frequency, recovery, cold-start, memory, and interpreter-only regressions
remain in the table and raw results. The data retains every paired sample,
range, workload/process wall and CPU metric, peak RSS, and cache observation.
The four cold controls have process CPU measurements, but no separate
workload CPU timer. Don't infer performance from native-coverage counts.

The full suite retains 24 unchanged fixtures with five pairs and nine startup/
import controls with 31 pairs. Its baseline is immediate a1e87c1a, isolating
the assertion change. The startup helper disables JIT.

The 23-workload JIT mean is 0.997565 of its predecessor and
3.398282 of CPython, with 6 of 23 time wins.
The historical 21-fixture cohort is 2.100196 of CPython,
or 2.101746 using ratios of medians.

All-24 process wall/CPU/RSS ratios to its predecessor are 0.998466,
0.996697, and 1.001719.
Peak RSS is 2.062068 of CPython, with 0 wins.

Both timing gates qualified; all later samples and load/VM/swap observations
remain. Keep the 21/23/24 cohorts distinct and use same-run paired ratios for
attribution. No universal, energy, controlled build-time, portability, or
free-threaded superiority is established. Large raw text is compressed
losslessly; the index verifies stored and uncompressed hashes and sizes.
Executables, frozen caches, and unselected temporary files remain local.
