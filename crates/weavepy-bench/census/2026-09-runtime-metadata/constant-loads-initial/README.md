# Initial JIT constant-load candidate

This intermediate release adds canonical string and tuple constant pins and
guarded tuple length. All 41 JIT tests, 283 VM tests, Clippy, workspace checks,
and compilation without the JIT pass. Focused identity and tuple-length
regressions pass under the JIT, the interpreter, and the GIL-disabled mode.
All five focused functions compile with no repeated exits. The preceding
release fails the new cold/hot string identity test, as retained in validation.

The call-overhead fixture still fails compilation on mixed arithmetic types
after getting past its tuple constant. Its trace is retained. The full 193-check
compatibility census has not run for this intermediate release. Measurements
here are focused comparisons with the small-slot release, not a full census.
The exact input sources, tracked diff, checksums, and local executable path
are retained so subsequent integer-result guard work can be isolated.

## Focused measurements

Five alternating measured cycles follow one discarded cycle. Ratios below
are candidate/reference; smaller is better. Workload time excludes setup,
and peak RSS covers the entire process. All five workloads are retained.

| Workload | Time/previous | CPU/previous | RSS/previous | Time/CPython | RSS/CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `tuple_literal_lengths` | 0.010 | 0.010 | 1.014 | 0.101 | 2.059 |
| `tuple_parameter_lengths` | 0.010 | 0.010 | 1.012 | 0.099 | 2.061 |
| `tuple_constant_returns` | 0.051 | 0.051 | 1.012 | 0.368 | 2.064 |
| `string_constant_returns` | 1.120 | 1.121 | 1.005 | 0.337 | 2.057 |
| `short_string_activations` | 0.884 | 0.884 | 1.004 | 4.368 | 2.060 |

Repeated string loads regress despite improved short activations. The
release retains a separate shared constant helper, adding an intermediate
call to the hot memo lookup. Inlining that helper is under investigation;
it has not been measured in this archived build.
