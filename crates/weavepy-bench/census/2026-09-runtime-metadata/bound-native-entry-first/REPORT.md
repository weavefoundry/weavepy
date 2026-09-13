# First post-binding native entry experiment

Status: **under refinement**. This is a complete measured experiment, not an
all-metrics acceptance. The performance goal remains unachieved.

Candidate ca6b541d uses already-bound positional slots to enter eligible native
functions before constructing an interpreter frame. Existing binding, live
defaults, observers, pending work, guards, argument lanes, and recursion checks
remain active. Frame buffers were already pooled, so this proves fewer frame
constructions and dispatches, not a measured count of removed heap allocations.
The executable is 44,290,720 bytes, 160 more than predecessor 539c6dd3.

All 275 standard compatibility checks, 60 targeted release checks, five extra
call/inspection/copy suites, and ten instrumented CPython value comparisons
pass. Instrumentation shows sparse-keyword and many-local framed entries fall
from 855 to 3 while direct entries rise from 2 to 854. An initial missing offline
dependency stopped the first build. The next full VM run passed 341 tests and
failed one coverage assertion that counted only framed entries. Values, key
identities, and compilations passed; the diagnostic recorded 2 framed plus 199
direct entries. Counting both kinds preserves the original threshold. That
corrected test passed separately, together with formatting, Clippy, and the
build without default features. A second complete VM run was not performed.

The focused protocol retains seven paired samples for ten call shapes and
four warm and four cold exception frequencies. All load preconditions qualified.
The original focused run completed 14 cases and then failed before any cold
timed sample because warm-only source files had no main timer. Exact original
cold sources were supplied in a separate declared cold-only continuation. No
completed case was rerun. The original failed stage, empty cold measurement,
logs, both gates, and the explicit completion manifest are preserved.

Sparse-keyword and many-local workload medians improved about 16.9 and 19.2
percent relative to 539c; all seven pairs won. However, excluded variadic
keyword, keyword-only, and closure cases regressed about 8.3, 10.2, and 5.9
percent; all seven pairs lost. These regressions require refinement. The earlier
always-raising regression was not recovered. All raw wall, CPU, RSS, and exact
value/cache results remain in the archive for both execution modes.

The complete full run qualified its load precondition and retained every later
sample even when load increased. It includes all 24 unchanged fixtures, five
pairs per execution mode, and nine startup/import controls with 31 pairs. The
startup helper disables JIT. Call-overhead workload time improved about 6.5
percent; the 23-workload geometric mean improved only 0.4 percent. List, numeric,
attribute, and generator controls include regressions. Aggregate process wall,
CPU, and memory are essentially unchanged. Small changes remain provisional.

The 23-workload JIT/CPython geometric mean is 3.4336, with 6 of 23 wins. The same
historical 21-fixture cohort is 2.1165 (2.1184 using ratios of medians). All-24
process peak RSS is 2.0732 times CPython, with zero wins. These distinct cohorts
must not be substituted for one another or described as universal improvement.

Generated-code inspection found that call_python_owned reserves 80 fewer stack
bytes than its predecessor; a larger stack frame does not explain the fallback
regressions. Changed register allocation or code layout is a hypothesis, not an
established cause. A narrower guard order will be tested independently.

The index hashes every selected artifact. Local executables, frozen caches, and
unselected intermediate outputs stay outside Git. These results do not establish
energy, controlled build-time, portability, or free-threaded superiority.
