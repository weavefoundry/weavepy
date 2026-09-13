# Runtime metadata and numeric text census

Git checkpoint note: see [CHECKPOINT-2026-09-13.md](CHECKPOINT-2026-09-13.md) for the latest active runtime, isolated experiments, measured improvements, and remaining gaps. The earlier [CHECKPOINT.md](CHECKPOINT.md) records the previous selection and local archive omissions.

The subsequent [native-call scratch screen](native-call-scratch/REPORT.md) is preserved but not retained. It improves attribute access by about 2 to 3 percent while regressing some controls. Follow-up coverage also shows that its original isolated shape probes do not exercise the changed native-to-native buffer path. The exact-decoder stage below predates the empty-table stage.

The original measurement checkpoint is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9` on
`perf/speed-up-json-and-strings`. The baseline executable was copied to
`target/release/weavepy-perf-9a69c41` before editing. Each environment file records
binary and source checksums. This work does not establish that WeavePy is faster
or uses less memory than CPython across all workloads.

The latest census that passed the strict load precondition is the [weakref key follow-up](weakref-shared-keys-followup/REPORT.md). Its 23 JIT workload timers have a geometric mean of 3.45x CPython; the original 21-fixture headline, including startup and excluding the three accelerator rows, is 2.12x. See the [cohort reconciliation](headline-cohorts/REPORT.md) for the comparison with the historical 2.91x result.

The subsequent [explicit-raise follow-up](jit-raise-exits-followup/REPORT.md) preserves a separate full diagnostic under host contention, including all regressions and the rejected prototype. Its focused guarded loops improve substantially, while always-raising calls regress. The full 23-workload mean is 0.4 percent slower than checkpoint 566c7bf's runtime, with 0.3 percent lower peak RSS; these small differences remain provisional. Its original 21-row timing cohort is 2.11x CPython by ratio of medians, and its complete 23-workload paired-ratio mean is 3.48x. It doesn't replace the preceding strict-load comparison.

The earlier collector-list census is [gc-traversal-lists/REPORT.md](gc-traversal-lists/REPORT.md). Removing copied collector lists reduces peak RSS on the large slotted and nested-tuple graphs by about 1.7 to 5.6 percent. The archive preserves all original and follow-up controls, every regression, complete compatibility and startup results, and exact source identities. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons; the overall goal remains unachieved.

The later [lazy finalization metadata experiment](gc-lazy-finalization/REPORT.md) is rejected. Its ordinary-heap memory savings came with about 4.2 percent higher finalizer-heap RSS and slower callback-heavy collection. The archive preserves all measurements, allocation profiles, source identities, and successful correctness checks. The preceding borrowed-handle change is now separately compared in the full-suite weakref follow-up; its focused collector measurements remain outstanding.

The [collector candidate-position experiment](gc-candidate-positions/REPORT.md) is deferred and its runtime change is removed. All correctness checks pass, but high and changing host load prevents a reliable performance conclusion. The archive retains all twelve cases, seven paired samples per case, observed regressions, OS memory context, and exact allocation-site evidence. It does not replace the latest complete census.

The preceding collector-index census is [gc-candidate-index/REPORT.md](gc-candidate-index/REPORT.md). The collector uses the existing address-mixing hasher for temporary candidate lookups. Focused batched full collections take about 40 to 58 percent less time than ab743. The archive includes before/after CPU samples, retained-object and cold-code controls, all regressions, and complete compatibility and startup results. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons; the overall goal remains unachieved.

The preceding shared-slot-name census is [shared-slot-keys/REPORT.md](shared-slot-keys/REPORT.md). For retained objects with 8, 9, or 16 slots, shared names reduce batch workload time by about 14 to 16 percent and peak RSS by about 6 to 8 percent with JIT enabled. The archive includes cold-code and setter controls, live allocation correlation, all regressions, and complete compatibility and startup results. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons; the overall goal remains unachieved.

The preceding field-validation census is [datetime-fields/REPORT.md](datetime-fields/REPORT.md). Valid exact-integer field checks reduce focused date/datetime construction time by about 40 percent, with 2 to 5 percent custom-index/Boolean fallback regressions. The archive records isolated-cache timings, corrected verification inputs, initial failures, and all remaining differences. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons; the overall goal remains unachieved.

The preceding parser census is [datetime-time-parts/REPORT.md](datetime-time-parts/REPORT.md). Supported ISO component parsing takes about 60 percent less time than e910f2dd. The whole datetime fixture remains essentially unchanged. The archive preserves corrected per-binary cache comparisons, the original shared-cache measurements and their recompilation limitation, all regressions, and validation outcomes. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons; the overall goal remains unachieved.

The preceding allocation census is [pickle-borrowed-strings/REPORT.md](pickle-borrowed-strings/REPORT.md). It retains bounded list snapshots and removes the extra borrowed-string allocation and copy. The archive records separate focused and full-census baselines, all measurements, compatibility results, and regressions. WeavePy wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons; the overall goal remains unachieved.

The preceding encoder census is [pickle-native-encode/REPORT.md](pickle-native-encode/REPORT.md). Supported ordinary payload encoding improves about 231 times versus the preceding release. The archive retains memory tradeoffs, configured fallback regressions, all raw measurements, and the original validation failures with two successful socket retries. WeavePy still wins 6/23 JIT workload timers and 0/24 JIT peak-RSS comparisons.

The preceding decoder census is [pickle-native-entry-guards/REPORT.md](pickle-native-entry-guards/REPORT.md). Supported ordinary payload decoding improves about 105 times versus the empty-table release, but still takes about 4.5 times CPython workload time. All 38 compiler, 328 VM, 52 JIT, 275 positive compatibility, and static checks pass. The archive retains all intermediate experiments, regressions, raw samples, and exact inputs. WeavePy still wins 6/23 workload timers and 0/24 peak-RSS comparisons.

The preceding empty-table census is [native-empty-tables/REPORT.md](native-empty-tables/REPORT.md). It reduces aggregate JIT workload time by about 1.9 percent, with a 17 percent attribute-access improvement and a 16 percent nested-loop improvement versus exact decoder. Measured peak RSS falls about 0.8 percent with JIT enabled and 0.5 percent in interpreted mode. All 38 compiler tests, 324 VM tests, 52 JIT tests, 272 positive compatibility checks, and static checks pass. Separate marshal/importlib diagnostics retain the preceding failures. The archive includes all 19 focused cases, 24 standard workloads, corrected native coverage gates, exact inputs, and regressions. WeavePy still wins 6/23 workload timers and 0/24 peak-RSS comparisons against CPython.

The preceding full census is [cache-decode-capacity/REPORT.md](cache-decode-capacity/REPORT.md). It reduces spare decoded-code capacity and import RSS, with explicit attribute and startup regressions.

## Measurement stages

- `before-thread-state/` contains the compact metadata cell measurements and
  142 compatibility checks. The executable is preserved locally as
  `target/release/weavepy-metadata-atomic`.
- `before-float-formatting/` adds thread-local dictionary bookkeeping and
  built-in type registry changes. Its complete 24-fixture suite uses five
  measured cycles. The executable is preserved locally as
  `target/release/weavepy-metadata-thread-state`.
- `before-json-key-memo/` adds direct JSON number output and the first stack
  float formatter. `float-stack.rs` retains that intermediate formatter.
  The executable is preserved locally as `target/release/weavepy-float-stack`.
  Seven-cycle numeric probes and a nine-cycle warm JSON/deque comparison are
  retained. Its float oracle matched the baseline on 62,376 values but exposed
  20 existing differences from CPython. At this stage, validation required
  equivalence to the checkpoint. The final oracle requires exact CPython parity.
- `before-enumerate/` adds the Zmij float converter and borrowed JSON key
  lookup. All 151 compatibility checks pass, and 262,376 float values match
  CPython exactly. The executable is preserved locally as
  `target/release/weavepy-runtime-metadata`.

- `before-boxed-iteration/` adds JIT lane inference for byte and generic
  enumeration. The executable is preserved locally as
  `target/release/weavepy-runtime-enumerate`. Generic primitive values still
  leave compiled execution at this stage.
- `before-direct-json/` permits immutable primitive values in generic pair
  loops to stay boxed in compiled execution. Both enumeration probes compile
  without repeated exits. The executable is preserved locally as
  `target/release/weavepy-runtime-boxed`.

- `before-byte-pairs/` constructs scalar strings from stack buffers, writes
  JSON integers with Itoa, and builds decoded dictionaries directly. Duplicate
  values are released before parsing the next field, matching CPython's callback
  timing. All 161 compatibility checks pass. The executable is preserved locally
  as `target/release/weavepy-runtime-final`.

The byte-pair stage reads native byte enumeration directly into compiled lanes,
avoiding temporary Python tuples. Counter overflow and other iterator shapes
retain the generic path. Known local object types retain their inferred
enumeration lanes without requiring a parameter probe. Its executable is
`target/release/weavepy-runtime-byte-pairs`.

`before-hot-layout/` preserves that complete measurement set. A nine-cycle
repeat confirmed 4–7 percent slowdowns in three interpreted workloads after
the byte-pair change. The interpreter's unchanged `step` and `call` bodies
had moved by 16 bytes. A broad 32-byte function-alignment experiment recovered
most of the loss but added 314,960 bytes to the executable. That compiler option
wasn't retained.

`before-tagged-append/` records a narrower experiment: three independently
aligned Mach-O sections for the interpreter loop, opcode dispatch, and call
dispatch. The sections apply only on macOS ARM64. The executable is preserved
as `target/release/weavepy-hot-layout`. It is 800 bytes smaller than the previous
candidate and improves the three repeated workloads by 3–6 percent, with
approximately unchanged peak RSS. Code placement is the suspected cause of
the regression; these measurements don't establish a universal alignment rule.
`before-native-list-reap/` preserves tagged scalar appends to generic list
builders. The builders compile without repeated exits and use less workload
time than CPython, but the byte builder exposed temporary list retention on
native frame exit. Its executable is `target/release/weavepy-runtime-tagged-append`.

`before-compact-caches/` contains the complete measurement and validation set
for native list cleanup, preserved as `target/release/weavepy-runtime-list-reap`.
All 169 compatibility checks pass. Cleanup removes the temporary-list memory
regression while preserving the compiled builders' speed improvements.

`before-tuple-pairs/` preserves compact full-range hash caches and inline
single-slot storage, with all 172 compatibility checks passing and ten focused
allocation/access workloads measured. Its executable is
`target/release/weavepy-runtime-compact-instances`. This stage has no separate
complete standard-suite census.

`before-small-tuples/` preserves direct native enumeration of immutable tuple
members, with all 172 compatibility checks passing. Its executable is
`target/release/weavepy-runtime-tuple-pairs`; the focused enumeration census
records its gains and the byte-enumeration regression.

`before-hash-normalization/` preserves the complete direct small-tuple census
and all 177 compatibility checks. Its executable is
`target/release/weavepy-runtime-small-tuples`. Fixed-size tuple construction
avoids an intermediate allocation, while bytecode tuples retain the free list.
The archive includes the measured source diff, test sources, measurement
scripts, checksums, and report. Scheduling anomalies and separate repeats
remain visible.

`before-tuple-hash-cache/` preserves the complete eight-byte hash-cache
census, all 181 compatibility checks, the measured source diff, tests,
scripts, and report. Its executable is
`target/release/weavepy-runtime-hash-normalization`. Six paired allocation
workloads use 1.4–3.6% less peak memory than the preceding release.

`before-small-slots/` preserves the completed tuple hash-cache census and
all 187 compatibility checks, including the nested C-API tuple suite's
14 cases. The preserved executable is
`target/release/weavepy-runtime-tuple-hash-cache`. Repeated tuple hashes
improve substantially, while retained tuples expose increased memory use.
Every focused workload and regression remains in the archive.

`before-constant-loads/` preserves the completed small-slot census, all
190 compatibility checks, exact input sources, and the report. Its executable
is `target/release/weavepy-runtime-small-slots`. Small slot vectors reduce
allocation and memory costs, while repeated access to the eighth field
regresses. That regression remains in the archive.

`constant-loads-initial/` preserves the focused constant-load experiment,
including its repeated-string regression. `scalar-guard-precision-failure/`
retains a subsequent correctness failure in native integer division. The
current candidate adds exact integer guards for generic call results,
keyword-gap fallback, and a precision guard for division. The completed census and all 196 compatibility checks are archived in
`before-unboxed-calls/`. The report identifies this scalar-result release by
checksum. Simple integer callbacks improve, but the broader call-overhead
fixture regresses by 18 percent and uses about 6 percent more peak memory
than the preceding release. `unboxed-calls-initial/` preserves the first completed-call optimization,
including all 199 compatibility checks and every focused measurement. It lowers
memory use in long call loops but doesn't recover the broader call regression.
`unboxed-calls-inline/` preserves the separate helper-inlining experiment, which
improves simple callbacks while increasing peak RSS by roughly 1%.

`scalar-leaves/` preserves bounded stack-buffer calls, all 202 compatibility
checks, complete focused measurements, and separate warmed nested-loop repeats.
`shared-artifacts-initial/` preserves the separate compiled-metadata sharing
build, its focused validation, and all focused measurements. Direct integer
calls improve substantially across these stages, while keyword and callable
controls expose regressions in the metadata-sharing stage.

A later audit reproduced an attribute-cache bug in the checkpoint and these
candidates: equal-length names with colliding hashes could hide an existing
attribute after a negative lookup. `exact-type-cache/` preserves the fix,
which compares exact owned names, all 205 compatibility checks, and a complete
24-fixture census. Its [report](exact-type-cache/REPORT.md) records that
completed stage. JIT workload time improves about 16% on
spectral norm and 6% on the call fixture versus the previous scalar-result
release. WeavePy still wins only 6 of 23 workload timers and 0 of 24 peak-RSS
comparisons against CPython. Top-level files are staging files; use each
completed archive's environment and samples together.

`indexed-slots/` preserves all 208 compatibility checks and complete focused
measurements. Name-checked ordered indices improve access to the eighth field
by about 21% in the interpreter and 62% in native code. The broad attribute
fixture improves about 7%; the call control regresses about 2%. Every result
remains in its [report](indexed-slots/REPORT.md). This intermediate stage has
no separate full standard-suite census.

`compact-caches/` preserves its complete full census and all 211
compatibility checks. Each inline cache shrinks from its measured 32 bytes
to 16 bytes, using checked 64-bit class-resolution tokens and exact name
checks. Class headers grow from 408 to 416 bytes. Geometric-mean peak RSS
falls about 2.7% with JIT enabled and 2.3% in interpreter mode versus indexed
slots. Native slot access improves about 8–10%, while the supplemental
eight-thread class-mutation workload regresses about 2.5% with the GIL
disabled. All samples and regressions remain in its
[report](compact-caches/REPORT.md). WeavePy still wins 6 of 23 workload
timers and none of the 24 peak-RSS comparisons against CPython.

`lazy-caches-initial/` preserves the next complete full census and all 214
compatibility checks. Allocating instruction caches on their first write
reduces geometric-mean peak RSS by about 2.3% versus compact caches, but
repeated cold and warm controls confirm slower interpreted string and list
workloads and an approximately 11% generator regression. Its
[report](lazy-caches-initial/REPORT.md) retains every result. This remains
an intermediate experiment; the speed regressions need further work.

`default-suffix-fix/` records a subsequent correctness repair for oversized
retained defaults after function code replacement. Debug and release checks
pass in all three execution modes, with separate proofs for scalar and
ordinary native frames. It has no separate performance census.

`atomic-cache-publication/` records the subsequent owning atomic-pointer table,
all 217 compatibility checks, and nine-cycle code, cold, and warm controls.
It recovers most of the interpreted generator regression, while string controls
remain slower than compact caches. Its [report](atomic-cache-publication/REPORT.md)
retains the preexisting shared-slot race diagnostic; that release does not yet
repair individual slot access.

`coherent-cache-slots/` records a complete 24-fixture census, with all 220
compatibility checks passing. Coherent atomic snapshots repair that shared-slot
race. Native 128-bit atomics keep slots at 16 bytes on this host; a nonblocking
24-byte fallback handles other targets. The CodeObject header remains 480 bytes.
The [report](coherent-cache-slots/REPORT.md) includes all focused, startup,
allocation, and parallel controls. Against the checkpoint, geometric-mean JIT
workload time falls about 15% and peak RSS about 5%. Against the immediately
preceding release, JIT time is approximately unchanged and interpreted time
rises about 1%. Concurrent class mutation also regresses. WeavePy still wins
only 6 of 23 workload timers and none of the 24 peak-RSS comparisons against
CPython. The archive preserves the failures, ordering models, exact source
inputs, and all regressions. Other free-threaded runtime limitations remain.

## Lazy instance dictionaries

The [lazy-instance-dicts archive](lazy-instance-dicts/README.md) contains the
full 24-fixture census and all 223 compatibility checks for release `67e7`. The
instance header remains 176 bytes while unused dictionary storage is deferred.
Nine-cycle focused controls show about 11% to 12% lower peak RSS and 16% to 19% less
workload time for retained cold instances. Exported and populated dictionaries
retain stable identity; their memory use is approximately unchanged. The
[report](lazy-instance-dicts/REPORT.md) records the public Rust field-type change,
ownership validation, cold/warm controls, all raw results, and regressions.

Aggregate standard-suite JIT workload time is approximately unchanged from the
coherent-slot release. Attribute access improves about 5% in cold and warm runs,
but several other timings and memory results regress. WeavePy still wins 6 of 23
workload timers and none of 24 peak-RSS comparisons against CPython. The user's
performance objective remains unachieved.

## Compact collector metadata

The [compact-gc-metadata archive](compact-gc-metadata/README.md) records the
complete census for release `b63eb22d`, including 226 passing compatibility
checks, 52 focused controls, collection-pause distributions, and repeated
empty-dictionary measurements. Collector handles shrink from 96 to 80 bytes.
Retained-container peak RSS falls roughly 2% to 3%, and several allocation
workloads get faster. Empty-dictionary allocation with normal collection
regresses in both the initial and repeated controls. Aggregate workload time
is approximately unchanged from the lazy-dictionary release.

The [report](compact-gc-metadata/REPORT.md) preserves these tradeoffs and a large
unexplained change in CPython's datetime timer. Separate diagnostics identify
two existing native slice defects in both the reference and candidate; they
are outside the 226-check suite and are not repaired by the collector change.
The archive includes their reproducers and execution traces, along with actual
object-address distributions for investigating the allocation regression.
The CPython-wide performance objective remains unachieved.

## Reproduction

Each completed archive contains the source inputs and scripts that produced its
results. Reproducing a historical stage requires those inputs in a separate
checkout of the checkpoint, including untracked source files. Building the
current working tree produces a new candidate, not a historical executable.
Keep new binaries and samples in a fresh output directory so preserved releases
and their checksums remain intact:

```sh
export WEAVEPY_STDLIB_CACHE="$PWD/target/performance-stdlib-cache"
census=crates/weavepy-bench/census/2026-09-runtime-metadata
output=$(mktemp -d "$PWD/target/performance-repeat.XXXXXX")
cargo build --offline --locked --release -p weavepy-cli --bin weavepy -j 1
cp target/release/weavepy "$output/weavepy"
python3.14 "$census/validate.py" --binary "$output/weavepy" \
  --out "$output/validation.json"
python3.14 tools/bench_compare.py \
  --previous target/release/weavepy-runtime-scalar-guards \
  --base target/release/weavepy-perf-9a69c41 --new "$output/weavepy" \
  --samples 5 --out "$output/suite.json"
python3.14 "$census/probes.py" \
  --base target/release/weavepy-perf-9a69c41 --new "$output/weavepy" \
  --samples 7 --out "$output/probes.json"
python3.14 crates/weavepy-bench/census/2026-09-execution/probes.py \
  --base target/release/weavepy-perf-9a69c41 --new "$output/weavepy" \
  --only startup startup_no_site imports --samples 31 \
  --out "$output/startup.json"
python3.14 "$census/parallel.py" \
  --base target/release/weavepy-perf-9a69c41 --new "$output/weavepy" \
  --samples 5 --out "$output/parallel.json"
python3.14 "$census/scalar_leaf_probes.py" \
  --base target/release/weavepy-runtime-unboxed-inline --new "$output/weavepy" \
  --samples 9 --out "$output/scalar-leaf-probes.json"
```

The reference executables above are preserved local builds identified by each
archive's environment file. Record fresh executable, source, and script hashes
for each new run. The historical summarizer checks its expected candidate and
comparison hashes; it must not combine results from different candidates.

Compilation and compatibility tests must finish before timing. Run measurements
on an otherwise idle machine. The harness checks that WeavePy has loaded the
complete staged standard library, alternates process order, and discards a
warmup cycle. It records each elapsed-time, CPU-time, and peak-RSS sample.
Candidate/reference ratios pair samples from the same cycle; ratios below one
mean less time or memory. Peak RSS includes setup and imports, even when the
workload timer excludes them. Supplemental probes use the interpreter mode.

The standard suite uses every existing work parameter. The separate `--warm`
comparison invokes the workload once before timing it. These measurements are
kept separate because they answer different questions. Build latency is recorded
in local logs but was not measured under controlled conditions.

`native-slice-bounds/` repairs two native slice defects: a minimum-integer stop
collided with the missing-bound sentinel, and two-bound fallback restored an
extra stack operand. Its release passes all 229 compatibility checks, 302 VM
tests, and 52 JIT tests, including required native execution and Unicode fallback.
Eight slice controls and five cold/five warm standard controls use nine paired
cycles. Two apparent slowdowns were repeated with 15 cycles; the original and
repeat results remain. This correctness stage does not repeat the full 24-fixture census.
The public Rust slice IR uses SliceOrigin to preserve producer shape; helper ABI
and matching release-library layouts remain unchanged. Binary/source checksums,
all raw data, native traces, and initial test setup failures are preserved.

`gc-index-mix/` changes only the collector's private address-to-handle hash map.
A final XOR distributes aligned addresses across more home buckets. All 229
compatibility checks, 303 VM tests, and 52 JIT tests pass. The full 24-fixture
suite, allocation controls, explicit GC pauses, startup/imports, cold and warm
workloads, and parallel measurements are preserved with exact inputs and hashes.
An isolated index experiment shows faster insertion and lookup but often slower
deletion; the report distinguishes those results from real VM measurements.
Dictionary and set gains were repeated for 15 paired cycles, with both runs
retained. Aggregate workload time is about 0.5 percent lower than the preceding
slice repair, while aggregate RSS is slightly higher. The CPython-wide goal
remains unachieved: 6 of 23 workload-time wins and no wins among 24 RSS controls.

`native-pin-retirement/` preserves the next measured intermediate. Exact string
lists can enter native loops, and frameless side exits release obsolete runtime
pins before their interpreter continuations. All 232 compatibility checks,
305 VM tests, and 52 JIT tests pass. Nine paired cold/warm controls show string
time improvements of about 26/22 percent, alongside an 86 percent cold-RSS
regression. That tradeoff is explicitly retained. The full census isn't
repeated at this stage; this intermediate does not satisfy the CPython goal.

`native-pin-pressure/` bounds temporary native roots at existing loop polls and
separates resource cleanup from speculative deoptimization accounting. All 235
compatibility checks, 306 VM tests, and 52 JIT tests pass. Nine paired cold/warm
controls, five-cycle string scaling, and 15-cycle attribute/numeric repeats are
preserved. Compared with GC-index, warm string RSS falls about 38 percent, but
cold RSS remains about 16 percent higher. Attribute execution regresses about
1.5 to 1.8 percent in repeats, with roughly 4 percent lower RSS. This stage
doesn't repeat the full census or satisfy the CPython goal.

`string-pin-budget/` checks temporary roots when exact string/list method results
are produced, using the existing completed-call reconstruction. Peak string RSS
falls another 9 percent relative to loop-poll pressure, with main time changing
by -1.2 percent cold and +0.6 percent warm. All 235 existing compatibility checks,
307 VM tests, and 52 JIT tests pass. New probes outside that set expose inherited
`str.replace` callback invalidation/frame defects and missing argument validation.
Their original failures and every performance sample remain in the archive.

`native-string-callbacks/` preserves a correctness-only release for `replace`
count callbacks. All 308 VM tests, 52 JIT tests, static checks, and focused
release probes pass. Its full compatibility/performance batch was not run.
A later probe finds untracked split-result lists that leak self-cycles; this
also limits what earlier negative sentinel-list observations could establish.
That collector repair precedes the next performance batch.

The [split-list tracking and activation cleanup stage](string-split-gc/REPORT.md)
repairs collector visibility, native entry-pin retention, parked-generator
cleanup, and escaped-frame local retirement. All 311 VM tests, 52 JIT tests,
and 244 configured compatibility checks pass. Five benchmark return-value
oracles agree with CPython on first and warmed calls. Nine paired cold/warm
cycles and five paired string-scaling cycles retain all samples: strings improve
about 3 to 4 percent against the callback repair, while list, attribute, call,
and numeric controls include regressions. At that stage, the full 24-fixture census remained
GC-index; the CPython-wide performance objective remains unachieved.


The [native string argument stage](native-string-args/REPORT.md) avoids a heap
allocation for common native string calls and corrects replace validation and
error precedence. All 313 VM tests, 52 JIT tests, 247 configured compatibility
checks, and static checks pass. The 58-case string oracle retains one known
split error wording difference; the other 57 rows match CPython exactly.
Focused cold/warm strings improve 3.3/3.9 percent against split cleanup, with
roughly unchanged peak RSS and a 1.6/1.5 percent attribute-time regression.
Its five-pair 24-fixture census records a full comparison, using the
original checkpoint and GC-index release as references. The JIT/CPython
geometric means are 3.251 for workload time and 2.096 for peak RSS,
with 6/23 time wins and 0/24 RSS wins.
The CPython-wide objective remains unachieved.

The subsequent [exact-range collection stage](range-collection/REPORT.md)
reserves known capacity and fills inline integers without repeated iterator
dispatch. All 316 VM tests, 52 JIT tests, 253 configured compatibility checks,
and 19 bounded release-oracle cases pass. Seven paired cycles retain five
cold and five warm controls, 12 small-constructor cases, and 20 retained
population cases. Large list/tuple construction takes 20–29/40–47 percent of
the preceding release time and beats CPython's construction timer at all five
populations. Small constructions remain slower than CPython. Peak process RSS
improves by up to about 5 percent but remains above CPython in every case.
The population checksum's temporary copy and the existing generic tuple
length-hook difference are recorded explicitly. The full census remains the
string argument stage; the CPython-wide objective remains unachieved.

The [streaming-sum stage](sum-streaming/REPORT.md) contains 256 configured compatibility checks, 50 focused cases, and a full 24-fixture refresh. A two-million-integer list workload beats CPython in elapsed time and peak RSS, while the standard census still wins only 6/23 workload timers and 0/24 peak-RSS comparisons. Float, mixed, large-integer, and range sum regressions are retained. Controlled launch-mode probes explain the CPython datetime shift between the two full censuses compared there; these ran outside and inside the tool filesystem sandbox, respectively. All original archives remain unchanged.

The [numeric-sum stage](sum-numeric/REPORT.md) adds exact float and large-integer sequence reduction. All 259 configured compatibility checks pass; 36 numerical oracle differences are fixed, and 46 existing differences remain. The six numeric sum timers beat CPython in a 15-cycle repeat, while their process elapsed time and RSS remain higher. All 50 original focused cases and 22 repeated controls remain available, including smaller integer/range/attribute regressions. That focused numeric stage didn't repeat the full census.

The [range-sum and iterator stage](range-formula/REPORT.md) contains 265 configured compatibility checks, 72 focused cases, and a full 24-fixture census. It repairs wide forward-range iteration and computes exact integer sums without walking the population. Large range timers beat CPython, while tiny-range speed and process memory remain higher. The standard census still wins 6/23 workload timers and 0/24 peak-RSS comparisons. All numerical/callback limitations and regressions remain recorded. Subsequent full results appear above.

The [weakref follow-up](weakref-shared-keys-followup/REPORT.md) records the post-checkpoint allocation profiles, startup controls, and complete workload comparison.

The [guarded pickle lookup follow-up](pickle-special-lookup-followup/REPORT.md)
was deferred. Several hook cases improved, but every metaclass-fallback pair
regressed and the standard JIT pickle case showed no clear benefit. Its exact
code, compatibility evidence, failures, and complete diagnostic samples remain
archived. The two runtime files were restored to the retained predecessor.

The [first post-binding native entry experiment](bound-native-entry-first/REPORT.md)
records complete qualified measurements and compatibility checks. Sparse keyword
and many-local calls improved, but excluded call shapes consistently regressed.
It is under refinement; the 3.4336 workload and 2.0732 peak-RSS ratios to CPython
do not satisfy the performance goal. Every original sample and failure is retained.

The [earlier native-entry exclusions](bound-native-entry-early-exclusions/REPORT.md)
pass the full correctness gate, but focused timing has wide dispersion and
retains fallback regressions. The complete focused and full measurements are
archived. This refinement does not establish that the regressions are fixed.

The [inline argument-binding flags experiment](argument-binding-flags/REPORT.md)
reduces small-signature flag storage and preserves large signatures. All
correctness gates pass, but the combined entry/storage change retains fallback
regressions. Both predecessor comparisons, all original samples, and the failed
probe that bypassed binding are archived. The performance goal remains unachieved.

The [startup memory diagnostics](startup-memory-regions-followup/REPORT.md)
separate RSS snapshots, shared mappings, virtual stack reservations, and live
allocator sites. Cold and warm cache conditions remain distinct. They identify
code/metadata and name-storage leads without claiming an RSS reduction.

The [outlined native-entry experiment](outlined-native-entry/REPORT.md)
improves several call controls versus its immediate predecessor but retains
fallback regressions against the earlier reference. Its complete focused and
full results, assembly, correctness checks, and load telemetry are preserved.
The performance goal remains unachieved.

The [compact intern-pool experiment](compact-intern-pools/REPORT.md) removes
duplicate string keys, improving intern-hit and marshal controls while reducing
string-pool memory. Complete paired results retain the short-string insertion
regression and the remaining CPython gaps. Large raw artifacts are compressed
losslessly, with stored and original hashes recorded.

The [assertion-exit experiment](jit-assertion-exits/REPORT.md) enables native
success paths around canonical exception constants. Its evidence includes
frequent-failure and recovery controls, all broader results, and a corrected
extra-suite filter that initially selected zero tests. CPython gaps remain.

The [guarded integer-comparison experiment](integer-comparison-guards/REPORT.md)
retains native success and fallback controls, full-suite results, and the
original analyzer-fixture failure. CPython gaps and individual regressions remain.

The [thin-owner prototypes](thin-owner-prototypes/REPORT.md) preserve standalone
ownership and Miri checks, including a correction that tuple owners also carry
wide slice metadata. They have not been integrated into WeavePy or benchmarked.

The [comparison call-fusion follow-up](comparison-call-fusion/REPORT.md) retains
simple-call gains, persistent complex-callback regressions, full-suite results,
and the corrected native-coverage fixture. Its decision is deferred.

The [shared value-storage follow-up](thin-value-storage/REPORT.md) records
16-byte values, broad memory savings, short-allocation regressions, and all
corrected validation and paired results. It remains a candidate under refinement.

The [compact metadata trial](compact-arc-storage/REPORT.md) preserves both
reference comparisons, smaller allocation headers, and shared-value cloning
regressions. It remains unfinished and motivates a narrower clone-path trial.

The [compact-owner clone follow-up](compact-arc-clone/REPORT.md) retains
focused gains, broader regressions, and a transparent correction of overlapping
measurement stages. Both packed-owner trials are deferred in favor of bf2.

The [aligned string-owner experiment](niche-string-owner/REPORT.md) preserves
validated ownership and compact layouts alongside measured runtime regressions.
It is deferred in favor of the retained bf2 implementation.

The [positional-default borrowing trial](borrowed-defaults/REPORT.md) records
consistent targeted gains alongside broader JIT regressions. It remains under
refinement; bf2 remains the retained acceptance reference.

The [combined-default borrowing trial](borrowed-keyword-defaults/REPORT.md) records
strong keyword and positional gains alongside remaining broader JIT regressions. It remains under
refinement; bf2 remains the retained acceptance reference.

The [native time-formatting trial](ascii-time-format/REPORT.md) preserves
correctness checks, focused gains and losses, and complete predecessor and retained-baseline comparisons.

The [short generic-call argument trial](native-dynamic-small-args/REPORT.md) preserves
call-path validation, stack tradeoffs, and complete direct and retained-baseline comparisons.

The [unobserved native-shell trial](native-unobserved-shell/REPORT.md) preserves
observer exclusion, ownership review, and complete direct and retained-baseline comparisons.

The [nested native scratch trial](native-nested-scratch/REPORT.md) preserves
ownership validation, recursion coverage, and direct and retained-baseline comparisons.

The [initial compiler scratch trial](jit-compiler-scratch-initial/REPORT.md) preserves
correctness validation, substantially lower retained heap allocations in three
recursive workloads, and a cleanup-stack tradeoff being refined before timing.
Its source and raw evidence are packed into verified archives; it has no timing
samples and does not establish a peak RSS or CPython performance improvement.

The [compiler scratch cold-reset trial](jit-compiler-cold-reset/REPORT.md) preserves
lower retained heap, static stack improvements, all four timing comparisons,
and the remaining process-time and peak-RSS tradeoffs.

The [shared terminal JIT-exit trial](jit-shared-exits/REPORT.md) preserves lower
peak requested compiler allocations, increased allocation activity, complete
correctness checks, and a timing gate that collected no samples because host
load stayed above its threshold. Runtime speed and peak RSS remain unmeasured.

The [early guard sealing experiment](jit-early-guard-sealing/REPORT.md) preserves all four runtime comparisons, compiler allocation tradeoffs, exact source identities, and the preexisting local-binding diagnostic.

The [native local writeback candidate](jit-local-writeback/REPORT.md) fixes unassigned locals on native exits and preserves full correctness and current census coverage evidence. Its runtime performance remains unmeasured.

The [object indexing experiment](jit-object-indexing/REPORT.md) preserves substantial focused integer-indexing gains, all four controlled comparisons, full semantic and native-path evidence, and the remaining startup, fallback, and peak-memory regressions. It does not establish an overall performance win.

The [exact builtin indexing candidate](jit-exact-indexing/REPORT.md) preserves callback-free indexing changes, complete correctness and census checks, and a load gate that collected no timing samples. Runtime speed and peak memory remain unmeasured.

The [keyword constructor replanning experiment](jit-keyword-constructors/REPORT.md) preserves expanded compilation coverage, three completed timing comparisons, construction-time regressions, and one unqualified gate. It remains an unsuccessful experimental candidate.

The [dynamic native object-return fix](jit-dynamic-object-return/REPORT.md) preserves the reproduced and corrected caller-retirement regression, complete correctness checks, remaining pin-pressure exits, and an unqualified timing gate. Its runtime speed and peak RSS remain unmeasured.

The [direct dynamic integer-return experiment](jit-dynamic-integer-return/REPORT.md) preserves zero temporary-integer pin-pressure exits, complete correctness checks, all four timing comparisons, and every measured regression. It does not establish an overall speed or peak-memory improvement.

The [observed integer arithmetic experiment](jit-observed-integer-arithmetic/REPORT.md) preserves substantial gains in six newly compiled arithmetic paths, complete correctness validation, all four comparisons, and the remaining memory, changed-type, and full-census regressions.

The [arithmetic call-result fusion experiment](jit-arithmetic-call-fusion/REPORT.md) preserves focused arithmetic gains, complete correctness checks, two completed comparisons, the unlaunched retained comparison, and the remaining full-suite speed and memory regressions.

The [scalar list-store candidate](jit-scalar-list-stores/REPORT.md) preserves native assignment coverage, exact type and finalizer checks, explicit socket-test reconciliation, and an unlaunched timing gate. Runtime speed and peak RSS remain unmeasured.

The [scalar-list cold-entry candidate](jit-scalar-list-entry/REPORT.md) preserves first-call native-store coverage, complete correctness validation, a separate JIT-enabled C API check, and an unlaunched timing gate. Runtime speed and peak RSS remain unmeasured.

The [scalar-list read experiment](jit-scalar-list-read/REPORT.md) preserves substantial focused speed gains, small peak-memory increases, complete correctness checks, every original and resumed timing sample, and explicit timer-wrapper and audit corrections. Full-suite and retained-baseline performance remain unmeasured.

[Datetime slot initialization](datetime-slot-initialization/REPORT.md) preserves a fully measured, unpromoted experiment: small constructor gains and lower retained-object RSS, slower custom-metaclass fallback, and no whole-suite gain. All source versions, validation outcomes, and measured samples are archived.
