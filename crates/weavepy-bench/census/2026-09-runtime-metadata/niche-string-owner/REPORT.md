# Aligned UTF-8 ownership with a reserved enum byte

Decision: deferred. Restore retained bf2 after preserving this record.
The candidate regresses workload time, process time, CPU use, and peak RSS
overall. Its local allocation benefits do not justify retaining the change.
Overall superiority to CPython remains unachieved.

Candidate: `6b4d28aa0f39d9cd64c863d954f9af8355811aab79c9b26ed5f21a3bdaadcd3b`,
44,137,120 bytes. Reference: retained bf2,
`bf2db3c58aae37ad8dee45c0859585a32af2cd44c97a2dcaed7a61f9f0df40e4`,
44,097,168 bytes. This is a same-run comparison against bf2 only.

Only shared_value.rs changes. UTF-8 text uses standard Arc<str> allocations
and aligned inline length metadata. A zero-only byte supplies invalid values
for enclosing Rust enum tags; exceptionally large lengths use individually
owned boxed Arc handles. Bytes, surrogate text, and tuples retain their thin
owners and allocation headers. Actual Object, Option<Object>, and DictKey
remain 16 bytes. SharedStr becomes 16 bytes; the other strong owners stay
8 bytes. The tuple payload remains 16 + 16*n bytes, excluding Arc counts.

Standalone layout and owning prototypes are preserved. Six ordinary tests
and six strict-provenance Miri tests on each of ARM64 and i686 passed, as
did clippy. The initial format failure was a missing tests.rs module that
had not yet been created; its log remains. Generated ordinary clone code
uses an aligned metadata load and no stack frame. Dereference includes an
exceptional-length check. These observations are not whole-VM speed proof.

Integrated validation passed 353 VM tests, 156 C API tests, strict lint and
format, a no-JIT check, all 87 targeted Python checks, and all 275 outside-
sandbox compatibility checks. Optional extension tests may check availability
internally. Exact storage modules passed 14 strict-provenance Miri tests per
architecture with documented Object/CachedHash stubs. This is not whole-VM
Miri validation. Bytes, codecs, and four extra C API suites passed. Marshal
received the expected divergence grade on both builds; the runner requires
the exact four enumerated IDs in expectations.toml, not an arbitrary failure.

All 336 source hashes and 78 method/input hashes remained unchanged through
the build and measurements. The observed release build took 4m04s, which is
not a controlled build-time comparison. Both timing stages used the exclusive
serial lease and unchanged host-load gate outside the filesystem sandbox.
The focused gate waited for load to qualify. Both stages then completed
sequentially. No measurement overlap or sample exclusion occurred.

Ratios below 1 favor the candidate. These means aggregate per-fixture
medians of paired ratios. Keep the 21-, 23-, and 24-row cohorts distinct.

| Metric | Candidate / bf2 | Candidate / CPython | Wins over CPython |
| --- | ---: | ---: | ---: |
| JIT workload time (23) | 1.017085943 | 3.416706531 | 6 |
| Interpreter workload time (23) | 1.015250718 | 9.403080794 | 1 |
| JIT process wall time (24) | 1.019841169 | 3.252681070 | 4 |
| JIT CPU time (24) | 1.019852172 | 3.321071360 | 4 |
| JIT peak RSS (24) | 1.004106795 | 2.019782839 | 0 |

The win column counts fixtures where the candidate beats CPython.
The 23 JIT workloads have 20 regressions against bf2. Twenty-two of 24 JIT
peak-RSS medians regress. All nine startup/import wall-time medians regress.
The focused shared string/bytes/tuple populations regress in every pair,
as does integer population. Nine-byte strings and shared dictionary population
use less peak memory in every pair; the overall tradeoff is unfavorable.

| Focused control | Time / bf2 | RSS / bf2 | Time wins (7 pairs) |
| --- | ---: | ---: | ---: |
| cold/shared_string_population | 1.044881 | 1.005578 | 0 |
| cold/shared_bytes_population | 1.064590 | 1.006602 | 0 |
| cold/shared_tuple_population | 1.049322 | 1.008142 | 0 |
| cold/integer_population | 1.104301 | 1.005089 | 0 |
| cold/shared_dictionary_population | 1.009702 | 0.981061 | 1 |
| cold/unique_ascii_7 | 1.000319 | 1.004823 | 2 |
| cold/unique_ascii_9 | 0.994886 | 0.931522 | 5 |
| cold/unique_ascii_17 | 0.995010 | 1.003723 | 4 |
| cold/unique_ascii_33 | 1.003735 | 1.003470 | 2 |
| cold/unique_ascii_129 | 1.000151 | 1.004674 | 3 |
| cold/unique_bytes_3 | 1.009290 | 1.006048 | 1 |
| cold/unique_bytes_9 | 1.012783 | 1.007082 | 0 |
| cold/unique_bytes_17 | 1.017936 | 1.003723 | 2 |
| cold/unique_bytes_33 | 1.003473 | 1.004854 | 2 |
| cold/unique_bytes_129 | 1.010486 | 1.003442 | 0 |
| cold/unique_unicode | 0.993546 | 1.004835 | 6 |
| cold/unique_surrogate | 1.031898 | 1.005031 | 0 |
| cold/unique_tuple_1 | 1.037230 | 0.997738 | 0 |
| cold/unique_tuple_2 | 1.018637 | 0.997854 | 1 |
| cold/unique_tuple_8 | 1.016845 | 0.997959 | 1 |
| warm/string_length_index | 0.995508 | 1.007230 | 4 |
| warm/bytes_length_index | 1.002243 | 1.007222 | 2 |
| warm/tuple_length_index | 1.023990 | 1.006111 | 0 |
| warm/surrogate_length_index | 1.005809 | 1.005540 | 2 |
| warm/shared_value_calls | 1.023066 | 1.003324 | 1 |
| warm/string_dictionary_lookup | 1.018030 | 1.007230 | 0 |
| warm/tuple_hash_lookup | 1.004069 | 1.007182 | 3 |

All raw samples, losses, validation outcomes, load observations, VM/swap
snapshots, methods, inputs, source snapshots, and code-generation artifacts
remain. Larger text artifacts use lossless gzip; index.json records both
stored and original hashes. Executables and frozen caches remain local.
Intermediate status notes are historical; this report records the decision.

No universal, energy, controlled build-time, portability, or native parallel-
scaling conclusion follows from these measurements.
