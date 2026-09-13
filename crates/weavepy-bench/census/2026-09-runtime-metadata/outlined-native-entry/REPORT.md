# Outlined native-entry eligibility

Status: **under refinement**. The overall performance goal remains unachieved.

Candidate 5b299d93 moves the existing post-binding eligibility predicates into
a helper with inlining disabled, then delegates to the existing guarded
native-entry routine. It adds no cache, allocation, object field, unsafe code,
or permanent deoptimization policy. It includes the preceding inline binding
flags and post-binding entry changes. The binary is 44,290,800 bytes, 80 more
than e507 and 240 more than retained 539c.

All 342 VM tests, formatting, Clippy, no-default-feature check, 63 targeted
release checks, 275 compatibility checks, five extra call/inspection/copy
suites, and native-entry coverage checks pass. The corrected many-body-local
control retains 854 generic calls and replaces 852 framed entries with direct
entries. All focused outputs agree with CPython before and after warmup.

Assembly confirms a separate helper and a tail branch to native entry. The
caller reserves 0x700 bytes after saved registers, versus e507's 0x6a0. The
proposed stack reduction did not materialize. An initial symbol search also
selected a closure and stopped before disassembly; corrected exact-symbol
captures are preserved. Stack reservation alone does not establish timing.

The same 28 focused cases use seven paired samples of both modes of the
candidate, immediate e507, retained 539c, and CPython. The table gives JIT
workload-time paired medians. Lower is better. Every sample, range, CPU,
process wall, RSS, interpreter-only result, and frozen-cache check is in JSON.

| Case | Time / e507 | Time / 539c | Time / CPython | Faster than 539c pairs |
| --- | ---: | ---: | ---: | ---: |
| calls/bound_method | 0.9726 | 0.9783 | 4.5134 | 5/7 |
| calls/builtin | 0.9573 | 1.0018 | 0.1399 | 3/7 |
| calls/defaults | 0.9924 | 0.9935 | 1.7116 | 4/7 |
| calls/method | 0.9768 | 0.9917 | 3.6240 | 6/7 |
| calls/positional | 0.9659 | 0.9819 | 2.0551 | 6/7 |
| calls/sparse_keyword | 1.0051 | 0.7895 | 12.7929 | 7/7 |
| calls/variadic_keyword | 0.9742 | 1.0430 | 15.5013 | 0/7 |
| calls/keyword_only | 0.9847 | 1.0632 | 15.5980 | 0/7 |
| calls/closure | 0.9753 | 1.0450 | 15.8648 | 0/7 |
| calls/many_locals | 1.0077 | 0.7806 | 7.5576 | 7/7 |
| signatures/defaults_16 | 0.9973 | 0.7934 | 9.3722 | 7/7 |
| signatures/defaults_17 | 1.0163 | 0.8241 | 9.4062 | 7/7 |
| signatures/defaults_64 | 1.0125 | 0.8141 | 6.9333 | 7/7 |
| signatures/defaults_130 | 1.0095 | 0.7751 | 4.9491 | 7/7 |
| signatures/keyword_only_16 | 0.9911 | 1.0318 | 8.2480 | 0/7 |
| signatures/keyword_only_17 | 0.9996 | 1.0562 | 8.5511 | 0/7 |
| signatures/keyword_only_64 | 1.0045 | 1.0140 | 16.5561 | 2/7 |
| signatures/variadic_14 | 0.9944 | 1.0388 | 6.1830 | 0/7 |
| signatures/variadic_15 | 1.0022 | 1.0577 | 5.9947 | 0/7 |
| signatures/many_body_locals | 0.9961 | 0.5967 | 0.6827 | 7/7 |
| exceptions/exceptions_none | 1.0231 | 1.0094 | 3.5740 | 2/7 |
| exceptions/exceptions_every_100 | 0.9873 | 1.0059 | 3.6267 | 3/7 |
| exceptions/exceptions_every_2 | 0.9926 | 0.9991 | 28.1441 | 4/7 |
| exceptions/exceptions_every_1 | 0.9983 | 1.0029 | 37.6542 | 2/7 |
| cold-exceptions/exceptions_none | 1.0019 | 1.0134 | 8.0408 | 3/7 |
| cold-exceptions/exceptions_every_100 | 0.9883 | 0.9903 | 7.0372 | 5/7 |
| cold-exceptions/exceptions_every_2 | 1.0013 | 1.0137 | 30.7123 | 0/7 |
| cold-exceptions/exceptions_every_1 | 0.9976 | 1.0005 | 39.4568 | 3/7 |

Several original call shapes improve about 2-4 percent versus e507. Sparse
keyword and many-local controls remain faster than retained 539c, while
variadic, keyword-only, and closure controls remain about 4-6 percent slower.
Some large-default controls regress about 1 percent against e507. This
experiment does not resolve those fallback regressions.

The full run uses all 24 unchanged fixtures with five pairs and nine startup/
import controls with 31 pairs. The startup helper disables JIT. The full
baseline is retained 539c, so this measures the combined entry/binding work.

The 23-workload JIT mean is 0.995951 of retained 539c and
3.397883 of CPython, with 6 of 23 time wins.
The historical 21-fixture mean is 2.096787 of CPython,
or 2.092741 using ratios of medians.

All-24 process wall/CPU/RSS ratios to 539c are 1.001413,
1.001174, and 1.002276.
Peak RSS is 2.074970 of CPython, with 0 wins.

Both timing gates qualified. All later load observations and samples remain
preserved. Compare candidates within the same paired run; different CPython
means across separate runs are not a controlled performance trend. Keep the
21/23/24 cohorts distinct. No universal, energy, controlled build-time,
portability, or free-threaded superiority is established.

The index identifies selected artifacts by SHA-256. Executables, frozen
caches, and unselected temporary files remain local and outside Git. The
proposed compact intern pools are a separate, unmeasured change.
