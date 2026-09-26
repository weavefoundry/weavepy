# Polymorphic attribute experiments

Two runtime candidates were tested against `1d0f79d`: borrowing exact cached
polymorphic field results during pure calls, and combining that change with
allocation of polymorphic tables only when a second field shape needs one.
Both runtime changes were rejected: the narrow gains do not justify repeated
startup and unrelated workload costs, and the combined candidate does not
reduce overall peak RSS. The runtime remains at `1d0f79d`; the portable probes,
semantic fixtures, and benchmark safeguards are retained for future work.

The allocation test distinguishes a reader first reaching its field after a
warm skip branch from an already-specialized monomorphic reader. Only the
former allocated an empty table in this test: 320 bytes for ten entries.
A property reader allocated four empty entries, or 128 bytes. The combined
candidate removes those tables until a real second field shape is recorded.
These are payload counts from isolated tests, not process-memory reductions.

Measurements use macOS x86_64, Rust 1.94, and CPython 3.14.5 with PGO, LTO,
and its tail-call interpreter. CPython's experimental JIT is disabled. Each
comparison interleaves variants, discards warmup, and verifies separate,
nonempty frozen caches that remain unchanged during timing. Builds, tests,
and profiles run separately from timed measurements. Ratios are medians of
matched cycles; lower is better.

| Full-suite metric | Borrow-only JIT/base | Combined JIT/base | Combined interpreter/base |
| --- | ---: | ---: | ---: |
| Workload time, 23 fixtures | 1.019 | 0.984 | 1.024 |
| Process elapsed, 24 fixtures | 1.020 | 0.984 | 1.026 |
| Process CPU, 24 fixtures | 1.020 | 0.987 | 1.029 |
| Peak RSS, 24 fixtures | 1.004 | 1.008 | 1.000 |

These are separate three-cycle suites and aren't cumulative speedups.
The combined suite remains at 1.070 times CPython's workload time,
1.335 times its process elapsed time, 1.278 times its process CPU time,
and 1.585 times its peak RSS. Geometric means don't establish superiority
on individual workloads.

The combined seven-cycle focused comparison gives JIT/base workload ratios
of 0.966 for DeltaBlue, 0.969 for Richards, 0.994 for attribute access, and
0.984 for call overhead. Its conditional polymorphic-method probe gives
0.943 cold and 0.966 after a full untimed invocation. That probe still takes
2.763 and 3.670 times CPython's workload time. Retaining 2,000 separately
compiled readers gives 1.086 for JIT time and 1.008 for RSS; the allocation
mechanism doesn't translate into a process-memory gain in this probe.

Long-workload rechecks use seven cycles at ten times the standard work count.
They remove the large initial Richards and interpreter-kernel slowdowns,
but retain costs in JIT kernels and attribute access:

| Workload | Combined JIT/base | Combined interpreter/base |
| --- | ---: | ---: |
| Richards | 1.007 | 1.002 |
| JIT kernels | 1.044 | 1.008 |
| Attribute access | 1.037 | 1.031 |
| Deque operations | 0.998 | 0.995 |

The borrow-only candidate's earlier eleven-cycle long rechecks retained a
1.036 JIT ratio for sorted-list construction (`fannkuch`) and a 1.030
interpreter ratio for attribute access. Other initial losses did not repeat.
These rechecks supplement the full suites; they do not replace their rows.

The borrow-only candidate's repeated 31-cycle startup comparison gives
JIT/base elapsed ratios of 1.019 for normal startup and 1.027 without site.
The combined 31-cycle comparison gives 1.010 normal, 1.044 without site,
1.023 isolated, and 1.003 for imports. Normal CPU is 1.041. The combined
normal/import elapsed ratios against CPython remain 1.446/2.692.

Both candidates passed full VM tests, Clippy with the existing local platform
exceptions, and release regression checks. The combined candidate passed
375 VM tests and 180 release runs across JIT, interpreter, and GIL-disabled
modes. Isolated tests verified borrowed-result ownership, mutation and
callback fallbacks, shared-cell rejection, and deferred allocation. Profile
samples confirmed the intended borrowed lookup ran in DeltaBlue; they don't
substitute for timing results.

The first borrow-only sweep inherited `PYTHONDONTWRITEBYTECODE=1`, disabling
frozen-cache writes. All its results were archived and excluded. Commit
`98d123b` adds a benchmark check that rejects an empty warmup cache and records
the environment flag. Benchmark controllers now use `python3.14 -B` to avoid
writing their own bytecode without passing the flag into measured children.
Generated samples, profiles, source snapshots, and binaries remain under
`target/performance/`.
