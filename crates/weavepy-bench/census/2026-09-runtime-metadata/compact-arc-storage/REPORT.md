# Compact metadata beside shared pointers

Decision: unfinished candidate under refinement. The packed representation
preserves smaller value slots and removes allocation length headers, but its
first implementation regresses shared-value cloning and broader JIT timing.
It does not achieve the user goal. Preserve both references for the next trial.

Candidate: `65ff081f8d85851c247cee8cc9e64bd3ebb1f80d49b617cbd6115ab44141c883`,
44,317,296 bytes. The primary thin-owner reference is bf2; the secondary
wide-owner reference is ed99. Full identities are in the candidate snapshot.

Actual ARM64 layout: Object, Option<Object>, and DictKey are 16 bytes;
strong shared owners are 15 bytes. Tuple payloads use 8 + 16*n bytes,
excluding Arc reference counts. These sizes alone are not timing or RSS results.

All 355 VM tests passed. The C API run passed 156 tests across 17 groups;
some optional extension tests check availability internally. Strict lint,
format, and no-JIT checks passed. All 87 targeted Python checks and all 275
outside-sandbox compatibility records passed. Extra bytes, codecs, and four
C API suites passed. Marshal retains the same four exactly graded divergences
on candidate and ed99; those are not clean passes.

The eight-test prototype passed ordinary tests and strict-provenance Miri on
ARM64 and i686. Exact integrated storage modules then passed 16 Miri tests on
each architecture, with Object and CachedHash stubs. This is not whole-VM
Miri validation or a formal soundness proof.

The first C API link failed with ENOSPC. Its log and the cleanup manifest
are retained. Only disposable target/debug output was removed, with no active
compiler or runtime; source, release references, caches, and results survived.
The subsequent fresh C API build and tests passed. The observed release build
took 4m04s; it is not a controlled build-time comparison.

All four predeclared load gates completed. Focused runs used seven pairs
for 20 cold and seven warm controls; full runs used 31 startup/import samples
and five pairs for all 24 fixtures. Every sample, loss, and later load
observation remains. Source and method hashes matched before and after timing;
all measured caches remained unchanged. No owned build/test/profile overlapped.

Ratios below 1 favor the candidate. Aggregation uses geometric means of
per-fixture medians of paired ratios. Keep the 21-, 23-, and 24-row cohorts
separate. Attribute changes to same-run reference pairs.

| Reference | JIT work / reference (23) | JIT work / CPython (23) | Wall / reference (24) | CPU / reference (24) | RSS / reference (24) | RSS / CPython (24) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| thin | 1.012205703 | 3.402394502 | 1.016652102 | 1.015741800 | 1.006426657 | 2.023999097 |
| wide | 1.007589126 | 3.405645854 | 1.014066301 | 1.013028646 | 0.982941311 | 2.021509717 |

Both full runs have six JIT workload wins against CPython and no peak-RSS
wins. Versus bf2, shared string/bytes/tuple populations regress 13.6-15.4%
in every JIT pair, while the integer population is nearly unchanged.
Nine-byte strings/bytes use about 6.9% less RSS in every pair. Warm calls
still regress. Versus ed99, all 27 focused RSS medians improve in every
pair, but all ten ASCII/bytes construction timings regress in every pair.

The next investigation is direct cloning of ordinary inline metadata while
preserving the reference count and the individually boxed escape path.
It is not implemented or measured by this archived candidate.

## Focused JIT results

| Control | Time / bf2 | RSS / bf2 | Time / ed99 | RSS / ed99 |
| --- | ---: | ---: | ---: | ---: |
| cold/shared_string_population | 1.135775 | 1.006616 | 1.059219 | 0.944709 |
| cold/shared_bytes_population | 1.154146 | 1.004580 | 1.030457 | 0.943487 |
| cold/shared_tuple_population | 1.148795 | 1.004571 | 1.050755 | 0.944551 |
| cold/integer_population | 0.995163 | 1.004061 | 0.858341 | 0.945428 |
| cold/shared_dictionary_population | 1.002105 | 0.982426 | 0.962725 | 0.887609 |
| cold/unique_ascii_7 | 1.018516 | 1.005219 | 1.052025 | 0.967829 |
| cold/unique_ascii_9 | 1.010788 | 0.931150 | 1.051293 | 0.967080 |
| cold/unique_ascii_17 | 1.009618 | 1.005214 | 1.034827 | 0.969838 |
| cold/unique_ascii_33 | 1.009552 | 1.003464 | 1.048598 | 0.969849 |
| cold/unique_ascii_129 | 1.007812 | 1.002953 | 1.033185 | 0.979362 |
| cold/unique_bytes_3 | 0.995443 | 1.006433 | 1.046832 | 0.967917 |
| cold/unique_bytes_9 | 0.996704 | 0.930649 | 1.045566 | 0.967454 |
| cold/unique_bytes_17 | 1.003049 | 1.007468 | 1.046972 | 0.972983 |
| cold/unique_bytes_33 | 0.989687 | 1.005557 | 1.040691 | 0.972110 |
| cold/unique_bytes_129 | 0.996473 | 1.003197 | 1.040798 | 0.979587 |
| cold/unique_unicode | 0.992154 | 1.004838 | 0.991690 | 0.969424 |
| cold/unique_surrogate | 1.012680 | 0.970187 | 1.023487 | 0.970853 |
| cold/unique_tuple_1 | 1.036403 | 0.997172 | 0.997290 | 0.848952 |
| cold/unique_tuple_2 | 0.984240 | 0.997320 | 1.014570 | 0.818362 |
| cold/unique_tuple_8 | 1.024883 | 0.998573 | 0.995464 | 0.787552 |
| warm/string_length_index | 0.988312 | 1.006659 | 0.986073 | 0.988017 |
| warm/bytes_length_index | 0.979731 | 1.007791 | 0.992994 | 0.987459 |
| warm/tuple_length_index | 1.001053 | 1.004992 | 1.009358 | 0.985877 |
| warm/surrogate_length_index | 0.987812 | 1.006663 | 0.977989 | 0.986921 |
| warm/shared_value_calls | 1.013060 | 1.006091 | 1.037954 | 0.986971 |
| warm/string_dictionary_lookup | 1.000629 | 1.009424 | 1.053244 | 0.986435 |
| warm/tuple_hash_lookup | 0.976030 | 1.005540 | 1.005822 | 0.986971 |

Full summaries retain every fixture, interpreter metric, cohort, and loss;
focused summaries retain all paired ratios and win counts. Large text and
JSON artifacts are losslessly compressed, with both stored and original hashes
in index.json. Comparison executables and frozen caches stay local. Exact
candidate source, changed bf2 preimages, method/input identities, and the
original validation/linker logs are preserved here. Earlier status notes
record historical intermediate states; this report gives the completed result.

No universal, energy, controlled build-time, portability, or native parallel
scaling conclusion follows from these measurements.
