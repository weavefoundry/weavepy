# Earlier exclusions for post-binding native entry

Status: **under refinement**. The performance goal remains unachieved.

Candidate 95b52f65 moves existing bound-signature and live-closure exclusions
before other code flags and omits two code-cell checks already enforced by
the memoized native-entry eligibility check. Generator families still take
the frame path. Native guards, lanes, observers, pending work, and recursion
checks remain active. No new cache, field, allocation, or unsafe code is added.
The release is 44,290,720 bytes, the same as ca6b and 160 more than retained 539c.

All 342 VM tests, formatting, Clippy, the build without default features, 60
targeted release checks, all 275 standard compatibility checks, five extra
call/inspection/copy suites, and ten CPython/native-coverage cases pass. The
full VM run includes the corrected framed-plus-direct coverage assertion.
Instrumented traces still establish entry-path coverage, not allocation counts.

The 18 focused cases retain seven paired samples for every variant. The run
met its load precondition, but one-minute load subsequently rose above five.
Paired timing ranges are wide in both execution modes. The result does not
establish that reordering the guards fixes the earlier fallback regressions.
All values and frozen caches passed. No sample was filtered or repeated.

The table reports JIT workload time and CPU relative to retained 539c. Lower
is better. Every paired value, range, execution mode, process metric, and
CPython comparison remains in focused/summary.json. Cold cases lack a separate
workload CPU timer; their process CPU measurements remain in the raw data.

| Case | Workload time / 539c | Workload CPU / 539c | Improved time pairs |
| --- | ---: | ---: | ---: |
| calls/bound_method | 1.0032 | 1.0161 | 3/7 |
| calls/builtin | 1.0360 | 1.0400 | 1/7 |
| calls/defaults | 1.0351 | 1.0344 | 2/7 |
| calls/method | 0.9756 | 0.9759 | 6/7 |
| calls/positional | 0.9626 | 0.9635 | 4/7 |
| calls/sparse_keyword | 0.8504 | 0.8506 | 5/7 |
| calls/variadic_keyword | 1.0244 | 1.0397 | 2/7 |
| calls/keyword_only | 1.2887 | 1.2550 | 1/7 |
| calls/closure | 1.1214 | 1.1194 | 2/7 |
| calls/many_locals | 0.6601 | 0.6848 | 6/7 |
| exceptions/exceptions_none | 1.0215 | 1.0109 | 2/7 |
| exceptions/exceptions_every_100 | 0.9996 | 0.9952 | 4/7 |
| exceptions/exceptions_every_2 | 0.9231 | 0.9260 | 6/7 |
| exceptions/exceptions_every_1 | 0.9784 | 0.9680 | 6/7 |
| cold-exceptions/exceptions_none | 1.1903 | n/a | 2/7 |
| cold-exceptions/exceptions_every_100 | 0.9640 | n/a | 5/7 |
| cold-exceptions/exceptions_every_2 | 1.0159 | n/a | 3/7 |
| cold-exceptions/exceptions_every_1 | 0.9989 | n/a | 4/7 |

The full census includes all 24 unchanged fixtures with five pairs and nine
startup/import controls with 31 pairs. The startup helper disables JIT. The
full load record and VM/swap observations are preserved. Meeting the initial
load limit does not imply an otherwise idle host throughout the run.

The 23-workload JIT geometric mean is 0.995867 relative to
539c and 3.396539 relative to CPython, with
6 of 23 CPython time wins. The same historical 21-fixture cohort
is 2.086196, or 2.082847 using ratios
of medians. These different cohorts must remain separate.

All-24 process wall/CPU/RSS ratios to 539c are 1.001130,
1.000132, and 1.001810. Peak RSS is
2.072558 times CPython, with 0 wins.

Generated-code inspection finds a 0x650-byte stack reservation after a 0x60-byte
register save in call_python_owned. That is 16 bytes less than ca6b and 96 bytes
less than 539c. This is a layout observation, not a timing explanation.

All selected artifacts are SHA-256 indexed. Executables, frozen caches, and
unselected intermediate outputs remain local and outside Git. These results
do not establish energy, controlled build-time, portability, or free-threaded
superiority. The separate argument-binder storage draft remains unvalidated
until a later candidate is explicitly built and measured.
