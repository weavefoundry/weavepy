# Direct cloning of compact metadata

Decision: deferred. Neither this clone refinement nor the preceding packed-
owner trial improves on the retained bf2 implementation overall. Return the
active runtime to bf2 after preserving this complete record. The user goal
remains unachieved.

Candidate: `75ea4029cf0b323ab53dfb7015485bfa2803b039779db62ba433fc9332972eb2`,
44,317,952 bytes. Direct reference is 65ff (packed owners); retained reference
is bf2 (thin owners). Full executable identities are in the snapshots.

Only shared_value.rs changes from 65ff. Ordinary clones increment the standard
Arc count and copy the existing pointer and metadata. An outlined cold helper
retains the exceptional boxed-owner behavior. Allocation, tuple, weak-owner,
and actual VM layout sizes remain unchanged. Object is still 16 bytes.

Both preliminary prototypes passed eight ordinary tests, eight strict Miri
tests on each of ARM64 and i686, and clippy. The first direct-copy prototype
kept its modeled 64-byte clone-wrapper frame; outlining the exceptional path
removed that frame from the ordinary standalone wrapper. Those are code-
generation observations, not runtime speed measurements.

Integrated validation passed 355 VM tests, 156 C API tests, strict lint and
format, a no-JIT check, all 87 targeted Python checks, and all 275 outside-
sandbox compatibility checks. Optional extension tests may check availability
internally. Bytes/codecs/four additional C API suites passed. Marshal retains
the same four exactly graded divergences against 65ff, not a clean pass.
Exact storage modules passed 16 strict-provenance Miri tests on both ARM64
and i686 with Object/CachedHash stubs. This is not whole-VM Miri validation.

The observed release build took 4m02s, not a controlled build-time result.
All 339 source hashes and 77 original method/input hashes remained unchanged;
the added measurement-controller correction is hashed separately.

## Measurement correction

The first focused run against 65ff completed in isolation. A later scheduling
error started the retained-reference focused stage while the direct-reference
full stage was still running. The attempted early stop found that a child
had already launched; the overlapping owned process tree was then stopped
with exit 143. No unrelated process was stopped. The original full run was
allowed to finish. Preserve both affected runs under overlap-diagnostics;
neither is an isolated headline comparison. No samples were removed.

The interrupted raw gate still reports running because its controller was
terminated. The incident record supplies terminal context, process IDs, and
timestamps. This stale JSON state does not mean a process is still active.

Before inspecting the affected full-run summary, the correction declared
fresh complete replacements due to the protocol violation, independent of
outcome. A new exclusive file lock and older-process check prevent overlapping
stages; a contention check rejected a second launch before creating any gate.
The corrected full-previous, focused-retained, and full-retained runs finished
sequentially in fresh isolated directories. The original source, inputs,
samplers, cache rules, and sample counts did not change. All original and
replacement samples, failures, and load observations remain.

## Isolated results

Ratios below 1 favor the candidate. Means use per-fixture medians of paired
ratios; keep the 21-, 23-, and 24-row cohorts distinct. These comparisons
provide no new direct comparison with ed99.

| Reference | Work / reference (23) | Work / CPython (23) | Wall / reference (24) | CPU / reference (24) | RSS / reference (24) | RSS / CPython (24) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| previous | 1.004360023 | 3.425429798 | 1.011941042 | 1.010376455 | 0.999364839 | 2.021447435 |
| retained | 1.014529570 | 3.411709694 | 1.019600944 | 1.018359493 | 1.005819609 | 2.019745994 |

Both full runs retain six JIT workload wins against CPython and zero RSS
wins. Against bf2, all 24 JIT peak-RSS medians regress. Shared string/bytes/
tuple populations remain about 10-11% slower in all seven JIT pairs. Repeated
shared calls and string dictionary lookups also regress in every pair.
The nine-byte allocation memory gain remains, but it does not compensate
for the broader losses. All interpreter and per-fixture results remain in
the summaries and raw measurements.

| Focused control | Time / 65ff | RSS / 65ff | Time / bf2 | RSS / bf2 |
| --- | ---: | ---: | ---: | ---: |
| cold/shared_string_population | 0.947423 | 1.002520 | 1.101447 | 1.006101 |
| cold/shared_bytes_population | 0.966772 | 1.001012 | 1.108797 | 1.004564 |
| cold/shared_tuple_population | 0.964006 | 1.000507 | 1.102782 | 1.005572 |
| cold/integer_population | 0.994410 | 1.001521 | 0.992276 | 1.006107 |
| cold/shared_dictionary_population | 1.005044 | 1.000000 | 0.992330 | 0.981781 |
| cold/unique_ascii_7 | 1.002397 | 1.000800 | 1.005010 | 1.004813 |
| cold/unique_ascii_9 | 0.993766 | 0.997604 | 1.012060 | 0.930906 |
| cold/unique_ascii_17 | 0.993791 | 0.998153 | 1.001931 | 1.004471 |
| cold/unique_ascii_33 | 1.006481 | 1.000692 | 1.000204 | 1.004506 |
| cold/unique_ascii_129 | 0.994081 | 1.000000 | 1.018197 | 1.004677 |
| cold/unique_bytes_3 | 1.001094 | 1.000000 | 1.003681 | 1.005229 |
| cold/unique_bytes_9 | 1.000156 | 0.999600 | 1.003053 | 0.932241 |
| cold/unique_bytes_17 | 1.018409 | 0.999260 | 1.000954 | 1.004836 |
| cold/unique_bytes_33 | 1.003532 | 1.000346 | 1.003269 | 1.004863 |
| cold/unique_bytes_129 | 1.007132 | 1.000736 | 1.004729 | 1.003198 |
| cold/unique_unicode | 1.005956 | 1.000741 | 0.992224 | 1.004469 |
| cold/unique_surrogate | 0.999666 | 1.000371 | 1.009514 | 0.970155 |
| cold/unique_tuple_1 | 0.996730 | 1.000852 | 1.004768 | 0.997170 |
| cold/unique_tuple_2 | 1.001945 | 0.999462 | 1.017409 | 0.997050 |
| cold/unique_tuple_8 | 0.973147 | 1.000817 | 0.992425 | 0.997960 |
| warm/string_length_index | 1.004245 | 1.001664 | 0.996721 | 1.007799 |
| warm/bytes_length_index | 0.991848 | 0.998894 | 0.998342 | 1.005562 |
| warm/tuple_length_index | 0.996852 | 1.002758 | 0.998371 | 1.007795 |
| warm/surrogate_length_index | 1.012562 | 1.000000 | 0.989975 | 1.004997 |
| warm/shared_value_calls | 1.005862 | 0.999451 | 1.022346 | 1.008315 |
| warm/string_dictionary_lookup | 0.998494 | 1.000549 | 1.027734 | 1.006641 |
| warm/tuple_hash_lookup | 0.988520 | 0.998899 | 0.972097 | 1.009424 |

Large text/JSON artifacts are losslessly compressed. index.json identifies
both stored and original bytes. Source snapshots, prototypes, assembly, methods,
inputs, validation, incident records, and all timing outcomes are preserved.
Comparison executables and frozen caches stay local. Earlier status notes
record intermediate states; this report gives the completed decision.

No universal, energy, controlled build-time, portability, or native parallel-
scaling conclusion follows from these measurements.
