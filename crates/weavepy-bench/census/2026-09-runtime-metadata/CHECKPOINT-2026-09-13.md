# September 13 performance checkpoint

This checkpoint preserves the active runtime, regression tests, and subsequent
experiments on `perf/speed-up-json-and-strings`. WeavePy has not achieved the goal
of outperforming CPython across all meaningful metrics.

## Measured improvements

- [Shared value storage](thin-value-storage/REPORT.md) reduced Object and DictKey
  from 24 to 16 bytes on ARM64. Against its immediate predecessor, the complete
  24-process census used about 2.34% less peak RSS geometrically, with all 24 rows
  improving. Shared-value populations used about 6% less RSS, and shared
  dictionaries used about 9.5% less. Some short allocations and calls regressed.
- [Time formatting](ascii-time-format/REPORT.md) reduced public `time.isoformat`
  time by about 37% to 39% against its predecessor. Some fallback cases regressed;
  the full suite was approximately flat.
- The separate [scalar-list read experiment](jit-scalar-list-read/REPORT.md)
  made warm indexed reads about 21.6 times faster than its predecessor and
  about 2.17 times faster than CPython on those focused cases. Warm iteration
  improved about 6.9 times against the predecessor but remained slower than
  CPython. Small peak-RSS increases remain. These experimental changes are
  archived and are not integrated into the active runtime.

These results come from different, documented predecessor comparisons. They
must not be multiplied together or presented as a single end-to-end speedup.

## Active code and experimental work

The active source tree is the [phase 79 scratch-capacity revision](native-scratch-byte-budget/REPORT.md).
All 340 source hashes match its validated build: 353 VM tests, 156 C API tests,
99 targeted checks, 44 independent CPython result checks, and 275 compatibility
checks passed, along with formatting, strict lint, and no-JIT checks. The final
scratch-capacity limit itself remains unmeasured because neither load gate
qualified. This is a progress checkpoint, not an acceptance claim.

Phases 80 through 96 are preserved separately with source archives, patches,
methods, validation, results where available, and explicit limitations. The
[phase 96 initializer guards](datetime-initializer-guards/REPORT.md) passed
380 VM, 156 C API, 79 JIT, 99 targeted, and 275 compatibility checks, plus all
24 census result comparisons. Its additional ordinary-subclass input preflight
and all performance measurements remain pending. No new timing run was started
after the checkpoint request.

The [latest fully measured experiment, phase 95](datetime-slot-initialization/REPORT.md),
took about **3.43 times CPython's time** over the 23-workload geometric mean
and used about **2.02 times CPython's peak RSS** over 24 processes. It won six
time comparisons and no peak-RSS comparisons. The older 21-workload cohort
was about 2.11 times CPython's time in that experiment; changing cohorts changes
the headline. Neither that result nor phase 95's slight full-suite regression
is a measurement of the active phase 79 capacity limit.

## What is saved

`checkpoint-2026-09-13-index.json` lists the exact source, test, report, archive,
and manifest files selected for this checkpoint, with hashes. The active
implementation is directly reviewable in Git. Source archives preserve isolated
experiments without silently promoting them into the runtime.

Older, previously uncommitted raw evidence is stored in each experiment's
`checkpoint-evidence.tar.gz`, with `CHECKPOINT-MANIFEST.json` recording every
included member's original bytes and hash. Extract these archives at the
repository root to restore their repository-relative evidence paths. For
later experiments, `sources.tar.gz` contains source-tree-relative paths and
should be extracted into a separate checkout; evidence archives contain the
original repository-relative method and result paths.

Some older archives contained generated Python caches. Their filtered
checkpoint copies preserve every other member byte and include the original
manifest. Historical REPORT and EXPORT files retain their original wording;
the checkpoint manifest identifies the committed archive and omitted cache
members. Original local files and archives have not been deleted or overwritten.

Build products, copied standard libraries, frozen bytecode, Python caches, and
allocation stack-log binaries are excluded. Earlier checkpoint omissions remain
documented in [CHECKPOINT.md](CHECKPOINT.md). This commit does not back up the
entire local `target/` directory or every historical diagnostic file.

Build the executable with `cargo build --release -p weavepy-cli`.
