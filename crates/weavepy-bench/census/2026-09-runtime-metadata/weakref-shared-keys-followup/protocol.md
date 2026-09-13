# Shared weakref keys and borrowed collector handles: broad controls

This protocol is declared before collecting startup or full-census samples for
the checkpoint release. All focused correctness checks and the full workspace
tests have passed. The existing 19-case diagnostic remains unchanged and its
timings remain provisional. The eight new allocation captures completed; their
instrumentation is used only for selected live-allocation evidence.

Compare the last full-census release, gc-traversal-lists (b9e58099), the following
gc-borrowed-handles release (36989234), and weakref-shared-keys (8899e367), plus
CPython 3.14.7. Retain binary SHA256 identities and exact source snapshots. The
intermediate release separates collector and weakref effects in this run.

Use the existing nine startup/import cases with 31 paired cycles, including
matched and relocated cached filenames. Use the unchanged complete 24-workload
suite with five paired cycles and its authoritative default work sizes. These
are the suite's original process-level runs, not a warm steady-state study.
Keep one declared unmeasured warmup cycle, alternating execution order, every
sample thereafter, and the separate per-binary frozen caches. Check that the
prepared caches stay unchanged during measured cycles. Measure elapsed wall
time, process CPU time, and OS peak RSS as supported by each existing harness.

Before launch, require one- and five-minute load averages at most four for
three consecutive ten-second observations, with a 600-second deadline. The
gate is context, not proof of an idle host. Preserve ongoing load and raw VM
and swap state. Keep all later samples even if load rises. Do not edit runtime
or active harness inputs, run concurrent builds/tests/profiles, exclude samples,
or retry a result for a better number. If the gate expires without launch,
preserve the failed gate and do not claim measurements occurred.

Compare both execution modes to CPython and each preceding WeavePy release.
Report unfavorable metrics and host interference alongside improvements.
These controls cannot establish universal superiority, energy use, controlled
build costs, free-threaded CPython performance, or parallel scalability.
