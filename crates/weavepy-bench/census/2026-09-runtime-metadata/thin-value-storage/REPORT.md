# Move slice lengths into shared allocations

Decision: retain as a measured candidate under refinement. The overall goal
remains unachieved. Short allocation and call regressions remain explicit.

The bf2db3c5 candidate uses one-word strong owners for Str, WStr, Bytes,
and Tuple. Slice lengths live in the immutable allocation, while ordinary
Arc controls reference counts and full Weak pointers retain metadata after
payload destruction. Tuple hash publication and unique free-list mutation
remain intact. Intern tables, iterator/buffer owners, native pins, C API weak
buffer caches, identity probes, and tracing owners use the new handles.
Compiler behavior and the independent non-atomic type-cache names are unchanged.
The JIT crate has only a storage-name documentation update in this experiment.

On ARM64, Object and DictKey are 16 bytes instead of 24. Shared handles are
8 bytes each. The extra per-allocation length word can increase short-object
allocator classes. The release binary is 44,097,168 bytes, 192,352 bytes smaller
than ed99. Layout and file size are not process-memory or execution-time results.

All 347 final VM unit tests, 34 C API unit tests, and nine loader integration
tests pass. Formatting, strict Clippy, and the build without JIT pass. Miri
passes eight tests on each of ARM64 and i686 using the exact storage modules
but stub Object/CachedHash definitions. This is not whole-VM Miri validation
or a formal soundness proof. Source, manifests, logs, and hashes are retained.

All 87 targeted release checks pass in JIT, interpreter, and GIL-disabled
modes. Standard validation passed 273 of 275 inside the tool sandbox; exactly
two loopback fixtures failed to bind. Both passed outside the sandbox with
the same binary, sources, and expected outputs. The combined 275-check file
preserves each original failure beneath its recheck. Other records retain
their original execution context. All oracle results remain available.

Additional bytes/codecs suites pass, as do four explicitly selected nested
C API bytes/Unicode/list/dict suites. Existing unittest skips are visible.
The initial nonexistent Unicode filter and empty nested selections are not
passes. The standard test_str suite covers Unicode behavior. The extra marshal
suite has the same four exact expected failure ids on ed99 and bf2: int/float
instance identity and two recursive tuple reconstruction cases. It is not a
clean marshal-suite pass. See the validation corrections and unchanged
expectations for the full limitation. All development failures remain.

The first Python hash test incorrectly assumed successful tuple hashes were
not cached; CPython 3.14 rejected that assertion. The corrected test checks
repeated hash failures followed by cached success. Both baseline and candidate
pass corrected semantics. No runtime or Python regression source changed
after the release build; only separately frozen validation/timing launchers
were corrected. Timing used only their v2 forms.

The original prebuild manifest has 336 source/configuration hashes and 75
method/input hashes. The executable snapshot includes 165 sources. Twenty
cold 200,000-value populations and seven warm 20,000-operation controls use
seven alternating pairs of ed99/candidate in both modes and CPython. All
values agree and frozen caches remain unchanged. GIL-disabled runs verify
correctness only. Every measured sample, range, loss, and host observation
is retained; one declared stabilization cycle is discarded per row.

| Control | JIT time / ed99 | JIT RSS / ed99 | Time wins / 7 | RSS wins / 7 |
| --- | ---: | ---: | ---: | ---: |
| cold/shared_string_population | 0.91739 | 0.93860 | 7 | 7 |
| cold/shared_bytes_population | 0.92775 | 0.93840 | 7 | 7 |
| cold/shared_tuple_population | 0.93226 | 0.93994 | 7 | 7 |
| cold/integer_population | 0.87246 | 0.93971 | 7 | 7 |
| cold/shared_dictionary_population | 0.97260 | 0.90495 | 7 | 7 |
| cold/unique_ascii_7 | 1.04170 | 0.96172 | 0 | 7 |
| cold/unique_ascii_9 | 1.03638 | 1.03981 | 0 | 0 |
| cold/unique_ascii_17 | 1.02812 | 0.96515 | 0 | 7 |
| cold/unique_ascii_33 | 1.03425 | 0.96841 | 0 | 7 |
| cold/unique_ascii_129 | 1.03260 | 0.97738 | 0 | 7 |
| cold/unique_bytes_3 | 1.05816 | 0.96146 | 0 | 7 |
| cold/unique_bytes_9 | 1.04738 | 1.03907 | 0 | 0 |
| cold/unique_bytes_17 | 1.05203 | 0.96588 | 0 | 7 |
| cold/unique_bytes_33 | 1.04249 | 0.97008 | 0 | 7 |
| cold/unique_bytes_129 | 1.04299 | 0.97764 | 0 | 7 |
| cold/unique_unicode | 1.02039 | 0.96624 | 0 | 7 |
| cold/unique_surrogate | 1.01131 | 1.00108 | 1 | 2 |
| cold/unique_tuple_1 | 1.00303 | 0.85115 | 2 | 7 |
| cold/unique_tuple_2 | 0.99830 | 0.82038 | 4 | 7 |
| cold/unique_tuple_8 | 0.95094 | 0.78829 | 7 | 7 |
| warm/string_length_index | 1.00251 | 0.98201 | 2 | 7 |
| warm/bytes_length_index | 0.99047 | 0.98145 | 5 | 7 |
| warm/tuple_length_index | 1.00078 | 0.97825 | 3 | 7 |
| warm/surrogate_length_index | 1.00084 | 0.98203 | 3 | 7 |
| warm/shared_value_calls | 1.01479 | 0.98153 | 0 | 7 |
| warm/string_dictionary_lookup | 1.00159 | 0.98046 | 3 | 7 |
| warm/tuple_hash_lookup | 1.01350 | 0.98045 | 1 | 7 |

The focused summary includes both modes and all workload/process wall/CPU
and peak-RSS fields. Cold controls retain process CPU; the unchanged sampler
extracts the separate workload CPU timer only for warm controls. Nine-byte
ASCII and bytes allocations lose about 3.9 percent RSS in all seven pairs.
Every ASCII/bytes construction control is slower in all seven pairs. Those
losses coexist with substantial shared-container and tuple memory savings.

The full run keeps 24 unchanged fixtures at five pairs plus nine startup/import
controls at 31 pairs. Both timing gates qualified and completed outside the
tool filesystem sandbox. All later load, VM, and swap data remain; there was
no sample filtering or automatic timing retry.

The 23-workload JIT mean is 0.997401768 of ed99 and
3.357302603 of CPython, with 6 time wins
and 12 workload medians slower than ed99.
The historical 21-fixture paired mean is 2.073226494
of CPython, or 2.071819837 as ratios of medians.

All-24 process wall/CPU/RSS ratios to ed99 are 1.006102889,
1.004819334, and 0.976611042.
Peak RSS is 2.010351456 of CPython with
0 wins. All full-suite RSS medians improve versus ed99.

Keep 21/23/24 cohorts distinct and use current same-run pairs for attribution.
No universal, energy, controlled build-time, portability, or native parallel
performance superiority follows. File-size and memory reductions do not erase
the retained execution-time and allocation regressions.

Large evidence files are compressed losslessly with stored and uncompressed
hashes. Executables, frozen caches, and unselected raw artifacts remain local.
Only indexed, explicitly allowlisted evidence is selected for version control.
