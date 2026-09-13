# Inline argument-binding flags

Status: **under refinement**. The overall performance goal remains unachieved.

Candidate e5077bba stores flags for up to 16 argument slots inline and retains
a heap slice for larger signatures. The flag extent includes positional,
keyword-only, and variadic slots, without covering unused body locals. Separate
flags preserve internal Object::Unbound behavior. Argument movement, default
handling, duplicate/missing checks, raw keyword ownership, and error precedence
are unchanged. The candidate includes the prior post-binding native-entry path.
The executable is 44,290,720 bytes, unchanged from immediate predecessor 95b.

All 342 VM tests, formatting, Clippy, no-default-feature build, 63 targeted
release checks, 275 standard compatibility checks, five extra call/inspection/
copy suites, and original native-entry oracles pass. The new boundary test
covers inline and heap limits, large signatures, variadic/keyword-only slots,
excess defaults, errors, and many body locals. Its baseline preflight matches
CPython, retained 539c, and 95b in all three checked modes.

Path inspection corrected the new many-body-local control before timing. V1
supplied every argument and bypassed the binder. V2 omitted a trailing default
but still bypassed it, failing the explicit generic-call coverage assertion.
Both returned correct CPython values; keep the failed coverage result. V3
supplies a/d and omits b/c in the middle. It records 854 generic dynamic calls
in both binaries. The other nine new source hashes are unchanged. V1/v2 inputs,
methods, traces, and failure remain archived. No timing sample was replaced.

The focused run has 28 cases and seven paired samples for seven variants:
both modes of the new candidate, immediate 95b, retained 539c, and CPython.
All exact values and isolated frozen-cache checks pass. The load precondition
qualified, and every later sample/load observation is retained. The table
reports JIT workload-time paired medians; lower is better. All CPU, process
wall, RSS, ranges, and paired samples for both modes are preserved in JSON.

| Case | Time / 95b | Time / 539c | Time / CPython | Faster than 539c pairs |
| --- | ---: | ---: | ---: | ---: |
| calls/bound_method | 1.0473 | 1.0213 | 4.6828 | 0/7 |
| calls/builtin | 1.0182 | 1.0252 | 0.1530 | 1/7 |
| calls/defaults | 0.9954 | 1.0059 | 1.8057 | 3/7 |
| calls/method | 1.0401 | 1.0049 | 3.8420 | 3/7 |
| calls/positional | 0.9901 | 0.9790 | 2.1053 | 4/7 |
| calls/sparse_keyword | 0.9724 | 0.7930 | 12.9552 | 7/7 |
| calls/variadic_keyword | 0.9959 | 1.0618 | 15.6318 | 1/7 |
| calls/keyword_only | 0.9726 | 1.0514 | 15.5882 | 0/7 |
| calls/closure | 0.9916 | 1.0595 | 16.0422 | 0/7 |
| calls/many_locals | 0.9794 | 0.7771 | 7.4127 | 7/7 |
| signatures/defaults_16 | 0.9821 | 0.7707 | 9.3071 | 7/7 |
| signatures/defaults_17 | 1.0162 | 0.8016 | 9.2311 | 7/7 |
| signatures/defaults_64 | 1.0032 | 0.8067 | 6.8249 | 7/7 |
| signatures/defaults_130 | 1.0012 | 0.7659 | 4.9566 | 7/7 |
| signatures/keyword_only_16 | 0.9920 | 1.0397 | 8.2705 | 0/7 |
| signatures/keyword_only_17 | 1.0016 | 1.0541 | 8.1429 | 0/7 |
| signatures/keyword_only_64 | 1.0036 | 1.0027 | 16.4619 | 3/7 |
| signatures/variadic_14 | 0.9775 | 1.0374 | 6.1962 | 0/7 |
| signatures/variadic_15 | 0.9900 | 1.0537 | 6.0737 | 0/7 |
| signatures/many_body_locals | 0.9726 | 0.5951 | 0.6454 | 7/7 |
| exceptions/exceptions_none | 1.0014 | 0.9998 | 3.6013 | 4/7 |
| exceptions/exceptions_every_100 | 1.0175 | 1.0101 | 3.7260 | 3/7 |
| exceptions/exceptions_every_2 | 1.0111 | 1.0101 | 29.1017 | 2/7 |
| exceptions/exceptions_every_1 | 1.0274 | 0.9965 | 38.9809 | 4/7 |
| cold-exceptions/exceptions_none | 1.0277 | 1.0208 | 7.7324 | 3/7 |
| cold-exceptions/exceptions_every_100 | 1.0031 | 1.0068 | 7.4428 | 2/7 |
| cold-exceptions/exceptions_every_2 | 0.9973 | 0.9950 | 30.2097 | 5/7 |
| cold-exceptions/exceptions_every_1 | 1.0023 | 1.0052 | 39.0970 | 2/7 |

Several keyword-call medians improve about 2-3 percent relative to 95b, but the
combined variadic/keyword-only/closure medians remain about 5-6 percent slower
than retained 539c. Those earlier regressions are not resolved. The corrected
many-body-local workload timer beats CPython; one added control is not a full-
suite, process-memory, or universal win. Cold exceptions have no separate
workload CPU timer; their process CPU samples remain available. No measured
malloc-count reduction is claimed because no allocation-history profile ran.

The full run retains all 24 unchanged fixtures with five paired samples and
nine startup/import controls with 31 pairs. Startup helper JIT is disabled.
Full-suite baseline is retained 539c, so it measures the combined change.

The 23-workload JIT mean is 0.988701 of retained 539c and
3.442987 of CPython, with 6 of 23 time wins. The
historical 21-fixture cohort is 2.117417 of CPython, or
2.119444 using ratios of medians. Keep those cohorts distinct.

All-24 process wall/CPU/RSS ratios to 539c are 0.993514,
0.993401, and 1.000465. Peak RSS is
2.072178 of CPython, with 0 wins.

The index identifies every selected artifact by SHA-256. Local executables,
frozen caches, and unselected intermediate outputs stay outside Git. These
results do not establish energy, controlled build-time, portability, or
free-threaded superiority. Any later outlining or memory-region experiment
is separate; neither is evidence for this candidate until actually measured.
