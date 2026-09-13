# Scalar stores into heterogeneous native lists

This validated candidate allows native indexed assignment of exact machine integers, floats, and booleans into a heterogeneous list. Runtime speed and peak RSS remain unmeasured: the first load gate timed out after 600 seconds without starting a child or collecting samples. The other three comparisons were not attempted. The primary source checkout and CLI were not replaced, and the candidate has not been promoted.

The analyzer now applies the existing scalar append compatibility rule to indexed stores. The generated store stages its exact value tag alongside its bits, and the existing helper decodes the scalar directly into the list without a temporary pin. Homogeneous integer and float list lanes remain exact. Bounds failures and finalizer-sensitive replacement still fall back before mutation; object-pin and None stores retain their behavior. The helper signature and frame layout are unchanged.

## Evidence and limits

The baseline compiler regression rejects the scalar assignment; the candidate passes. All six new release scalar-store kernels compile, while their predecessor versions are rejected. Both homogeneous-list controls compile in both binaries. All 29 focused release oracles match CPython, including the 21 unchanged phase-91 bodies.

The exact-type VM test requires at least 2,000 completed native stores for each of int, float, and bool, including negative indices. Separate tests verify out-of-range errors, exactly-once value/index callbacks in their original order, and a displaced object's finalizer observing the new list value. The finalizer test's required native-store count exceeds all possible warm-up stores. Test-only counters are absent from the release binary.

Source01 passed the compiler suite and exact-type test but failed to establish native execution in the combined fallback test. An unchanged trace found an opaque index and a homogeneous parameter-list path. Source02 fixed the index lane and separated finalizer coverage; source03 retained a heterogeneous list shape. Their single cold finalizer call still recorded no new-path stores. Source04 added three short warm-up calls, with an assertion that requires native execution in the finalizer run itself. The production implementation is unchanged across those test refinements. All versions and failures are retained.

Validation passed: 79 JIT tests, 368 VM tests, 156 C API tests, formatting, strict all-target clippy, no-JIT checks, 99 targeted release checks, 44 fixture paths, and all 24 census checksums. Compatibility initially produced 273 successes and two EPERM failures at local socket binds in the tool sandbox. Only those two fixtures were rerun outside the sandbox, both passing exact expected-output checks. A separate reconciled report records 273 unchanged successes plus two rechecks, preserving all original failures and per-row provenance. No automatic approval-review rejection occurred. Preexisting staticmethod class-subscription and frame-identity differences remain explicit.

The release binary is 44,098,368 bytes, unchanged from phase 91. Its SHA-256 is ce054fa7ac4c1f287098d904b73af74f44bfc44fd7a18ce131fc26322fa26124. The observed 5m 36s build duration is not a controlled build-performance result. Static codegen and prologues are preserved without treating them as cumulative stack or RSS measurements.

## Measurement status and next work

The frozen timing plan contains 29 focused cases: the unchanged 21 phase-91 bodies, six scalar-store cases, and two homogeneous-store controls, all at 20,000 operations. Cold and warm runs are separate, with seven paired samples planned per case. The unchanged full census has 24 fixtures with five paired samples; nine startup/import cases have 31. New focused subgroups must not be compared with an older aggregate as though the cohorts were identical. None of these phase-92 samples was collected.

Release scalar-store probes report zero OSR entries, despite compilation and validated warm native execution. The existing ListObj entry guard refuses scalar first elements. Reads and iteration on the same lane also fall back for scalar values. These checks preserve correctness but limit cold execution and store-then-read workloads. The next investigation will reproduce the entry restriction with a single long call before changing it; any scalar-read expansion needs separate coverage and pin-pressure checks.

Complete source snapshots, all four test-source versions, baseline rejection, native and release checks, original and reconciled compatibility records, exact diagnostic PCs/cache states, raw outputs/traces, codegen, load telemetry, input bodies, and methods are archived. Executable binaries and regenerable build/stdlib/frozen caches are excluded. A narrow, inventoried cleanup removed completed project artifacts while preserving target roots, dependencies, sources, logs, references, and the active candidate target.

No performance gain or universal superiority is established by this candidate. The goal remains unmet, including peak memory and unmeasured energy, scaling, portability, and controlled build costs.
