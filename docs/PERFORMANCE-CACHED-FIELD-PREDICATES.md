# Decode cached field predicates once

Small predicates such as `left.value < right.value` already qualify for pure-leaf
execution. Their bytecode shape is now recognized once alongside the existing tiny
return shapes. On a valid instance-dictionary cache hit, the evaluator reads the two
fields and compares them without its general operand stack or owned-value scratch.
The fields remain borrowed from live arguments; this path runs no Python callbacks.

The decoded path shares the general evaluator's existing comparison rules, including
exact integers, floats, booleans, strings, and safely representable mixed integer/float
pairs. NaNs, large mixed integers, rich comparisons, stale caches, descriptors, and
other unsupported shapes retain the existing fallback. Class versions, requested field
names, current code/default bindings, observers, recursion, and shared-storage checks
remain in place. No JIT admission, compilation budget, workload, or gate changes.

An earlier integer-only candidate is held: its seven-cycle float probe regressed to
1.089 times baseline JIT work and 1.110 times interpreter work. The revised path checks
unsupported cache kinds early and reuses the existing comparison rules. Its raw
predecessor measurements and source snapshot remain available; they are not treated as
successful results.

## Validation

All 386 VM tests, the unchanged 1 MiB embedding test, VM Clippy with its two existing
exclusions, scoped formatting, no-default-feature compilation, and 14 benchmark-tool
tests pass. JIT sources are unchanged. The frozen release passes 279 regression runs
across 93 fixtures in JIT, interpreter-only, and GIL-disabled modes, plus 352 probe
checks. The new fixture covers all six comparisons, operand order, numeric boundaries,
non-boolean rich results, descriptor callbacks and caller frames, mutation, exceptions,
default/code replacement, tracing, and collection. A test-only counter records 1,780
uses in two complete DeltaBlue iterations. Subsequent macOS and Windows CI runs
pass the Python assertions but report zero hits in the JIT-enabled coverage test.
The test now executes both modes and requires these interpreter-path counts with
native compilation disabled, avoiding dependence on native dispatch decisions.
That mode records 1,822 DeltaBlue hits locally; both modes retain the semantic checks.
Windows CI for `c3b2bd4` still records zero hits with the JIT disabled, so native
dispatch alone doesn't explain the discrepancy. The unchanged coverage assertions
now print the affected function's classification, bytecode, and inline caches on
failure. Diagnostics from `3e1453d` confirm the expected predicate shape and warmed
instance caches on Windows. Further test-only counters distinguish call guards,
operand-release admission, evaluator entry, and field hits. The original hit
snapshot and assertion remain unchanged; diagnostic calls can't satisfy coverage.
The Windows cause remains unresolved. macOS and Linux unit CI pass on `3e1453d`.

The binary is 51,872,944 bytes, 4,208 bytes larger than baseline `a7b67f6`. Its SHA-256 is
`58e904115755973c50311371967a0c96ca111e863bcacf522fda8ec9770c628d`; the baseline is
`073b21341586b79c78db5f11c42b48fc4b334e0d53e607a7faeb827d20bb7512`. The release build
completed and its controller closed before freezing the executable. Sources, hashes,
immutable binaries, and raw data remain under
`target/performance/cached-field-predicates-investigation/` and
`target/performance/cached-field-predicates-*`.

## Measurements

Measurements use macOS x86-64 and optimized CPython 3.14.5, with the unchanged paired
methodology in [Borrowed scalar calls](PERFORMANCE-BORROWED-SCALAR-CALLS.md). Setup and
result validation remain timed. The reusable probe covers integer, slot, float,
class-attribute, string, and mixed-value predicates. Standard probes use seven paired
cycles and 200,000 comparisons; sustained probes use five cycles and one million.
Separate traces have the same JIT compilation decisions for all six cases.

Ratios below are candidate/baseline; lower is better.

| Field kind | Standard JIT work | Process elapsed | Process CPU | Peak RSS | Sustained JIT work | Sustained JIT/CPython work |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Integer | 0.884 | 0.948 | 0.947 | 1.004 | 0.871 | 1.485 |
| Slots | 1.046 | 1.014 | 1.007 | 1.002 | 1.040 | 7.009 |
| Float | 0.856 | 0.941 | 0.962 | 1.001 | 0.864 | 1.458 |
| Class | 1.046 | 1.014 | 1.028 | 1.002 | 1.035 | 2.898 |
| String | 0.872 | 0.910 | 0.891 | 1.001 | 0.863 | 1.321 |
| Mixed | 0.817 | 0.916 | 0.900 | 0.997 | 0.864 | 1.231 |

The supported dictionary-field cases improve by 12-18% at standard work and 13-14%
at sustained work. Unsupported slot/class cases retain costs of 4.0%/3.5% in sustained
JIT work and 6.0%/3.7% in interpreter work. Those regressions remain open. The new
shortcut still lacks a slot-cache read; that is the next independent target.

Three cycles of the unchanged 24-fixture suite give default-JIT geometric means of
0.996 workload time, 0.968 process elapsed, 0.972 CPU, and 1.005 peak RSS.
Interpreter-only means are 0.990, 0.981, 0.980, and 0.993. Workload means exclude
startup. Default-JIT means against CPython are 1.077, 1.358, 1.271, and 1.564,
respectively. Overall CPython parity remains unachieved.

DeltaBlue's seven-cycle focused JIT ratio is 1.010; the three-cycle broad ratio is
0.985. These measurements do not establish a repeatable application-level gain.
Its focused RSS ratio of 1.038 is 0.988 in the broad run. Richards, call-overhead,
and attribute-access focused JIT ratios are 0.999, 1.010, and 1.013.

Initial nested-loop and spectral-norm JIT ratios of 1.040/1.023 repeat at 0.984/0.987
in seven cycles. Initial pidigits RSS of 1.040 repeats at 0.974. String interpreter
work retains a smaller cost, 1.021 initially and 1.013 on repeat. JSON/list peak RSS
of 1.040/1.034 repeats at 1.038/1.042, prompting the separate 31-cycle checks below.
All original measurements remain recorded, and rechecks do not replace the broad
suite means.

Thirty-one startup cycles give default-JIT elapsed ratios of 0.985 ordinary,
0.989 no-site, 1.000 isolated, and 0.992 imports. A separate complete 31-cycle repeat
gives 0.987, 0.997, 0.985, and 0.988, respectively. The initial no-site CPU ratio
of 1.038 repeats at 1.008. Repeated startup RSS stays within 0.4% of baseline.

A separate 31-cycle JSON/list comparison at the original work sizes gives RSS
ratios of 1.002/1.001 and JIT workload ratios of 1.007/0.995. A subsequent 31-cycle
identical-baseline-binary control gives RSS ratios of 0.971/1.004 and workload ratios
of 1.005/0.997. The larger initial memory increases do not repeat in the longer
comparison, and the identical-binary result confirms material RSS variation. This
does not erase the original outliers or establish an RSS improvement.

The preceding `a7b67f6` head has 21 passing CI checks, six ecosystem checks still
running, and the Windows benchmark gate failing. Linux and macOS benchmark gates
pass. The Windows gate retains a 1.218 cold sumvm ratio against the merge base;
separate diagnostic cold sumvm/nested-loop/jitloop ratios are 1.209/1.157/1.161,
versus warm ratios of 0.998/1.002/1.003. Complete process elapsed improves, but the
first-use compilation cost remains unresolved.

## Release-guard follow-up

The `949d5df` macOS and Windows diagnostics identify instance-release rejection
after successful predicate classification and binding. A local reproduction
finds a stale positive collector filter on an untracked instance with two owners.
The [deferred-instance release fix](PERFORMANCE-DEFERRED-INSTANCE-RELEASES.md)
uses the existing exact deferral flag while retaining last-owner and tracked-object
cleanup checks. Cross-platform validation and performance measurements are pending;
the earlier measurements above remain the accepted baseline.
