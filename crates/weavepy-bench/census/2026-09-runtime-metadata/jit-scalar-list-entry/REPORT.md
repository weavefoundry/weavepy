# Cold native entry for scalar-containing lists

This validated candidate lets native loops enter when a heterogeneous list begins with an exact machine integer, float, or Boolean. Its runtime speed and peak RSS remain unmeasured: the first load gate expired after 600 seconds without launching a child or collecting samples. The other three comparisons were unattempted. The primary source checkout and CLI have not been replaced, and the candidate has not been promoted.

The production change extends the first-element entry check for ListObj to the three scalar types already accepted by native append and indexed stores. Exact-list and homogeneous integer/float lane checks remain in place. Read and iteration helpers still fall back for scalar elements on the object lane. No helper ABI, frame layout, allocation policy, or release counter changes.

## Correctness and native execution

The baseline reproducer produces the right values but completes zero native scalar stores during its first 4,096-iteration call. The candidate's integer, float, and Boolean cases each complete 4,095 native stores and one OSR entry, with exact expected values and interpreter agreement. Each case has a single call and a minimum requirement of 4,000 native stores, so warm-up work cannot satisfy it.

All six unchanged release scalar-store probes now report one OSR entry, compared with zero in phase 92. All 29 focused bodies and work values are byte-identical to phase 92, and every result matches CPython. Both homogeneous-store controls still compile. The inherited constructor and arithmetic pin-pressure checks also pass. Native entry proves execution coverage, not a speed or memory improvement.

Validation passed: 79 JIT tests, 369 VM tests, 156 C API tests, formatting, strict all-target clippy, no-JIT checks, 99 targeted release checks across 44 fixture paths, all 275 compatibility checks, and all 24 census checksums. The complete release pipeline ran outside the filesystem sandbox, including its loopback fixtures; this phase needs no compatibility reconciliation.

The regression controller deliberately disables JIT for interpreter-reference calls; dedicated VM JIT workers explicitly enable it. That default also propagated into the full native controller. A separate, preserved run of all 156 C API tests with WEAVEPY_JIT=1 passed, supplementing the original interpreter-default C API run. Preexisting staticmethod class-subscription and frame-identity differences remain documented in the semantic evidence.

## Measurement status

The release binary is 44,098,368 bytes, unchanged from phase 92. Its SHA-256 is e8ac09bb3ceaf7ee65b59b18722893ec0d269721103cd38f5d031820c21edd67. The observed 4m 47s build duration is not a controlled build-performance measurement. Codegen and prologue inspection are retained without interpreting them as cumulative stack or peak RSS measurements.

The frozen plan contains 29 focused cases at 20,000 operations, seven paired cold and warm samples each, the unchanged 24-fixture census with five pairs, and nine startup/import cases with 31 pairs. Both the immediate predecessor and retained phase-69 reference are specified. Six focused subgroups remain separate. No phase-93 timing samples were collected, and no aggregate can be reported for them.

The first gate ended with status not_started and exit code 75. Its complete load telemetry is retained, and no launched samples were excluded or retried. Other project build/test activity was visible during the wait. The remaining three stages were not launched.

The archive retains all 340 source identities, the two-file patch, expected baseline failure, candidate regression, full validation, the separate JIT-enabled C API run, raw outputs and native traces, codegen, frozen input/method identities, and the unlaunched gate. Executable binaries and regenerable dependency/build caches are excluded. The inventoried cache cleanup preserved source, logs, references, and the active target.

The next bounded investigation is scalar reads and iteration on the heterogeneous list lane, including native-execution assertions, changed-type and mutation behavior, and pin-pressure handling. No overall performance gain or universal superiority is established; the user's goal remains unmet.
