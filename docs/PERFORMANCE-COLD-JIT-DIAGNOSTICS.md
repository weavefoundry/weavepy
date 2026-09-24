# Cold JIT diagnostics

At runtime commit `472e26f`, the Windows performance gate retains sumvm and
jitloop regressions of 19.2% and 15.1% against the merge base after retries.
The Linux and macOS gates pass. Earlier local measurements suggest that first
compilation can be a substantial part of short numeric workloads, but they
don't identify the Windows cause.

`tools/bench_cold_jit.py` collects diagnostic evidence separately from the
unchanged admission gate. It compares the existing sumvm, nested_loops, and
jitloop fixtures in fresh processes and on a second call within one process.
Both workload time and whole-process elapsed time are retained. Compilation
traces use separate instrumented runs, with markers around the first and
second workload calls. The trace driver avoids importing runpy or types before
the first call, since those imports could warm the compiler themselves.

The tool alternates baseline/candidate/CPython order, discards a warmup cycle,
and verifies that each WeavePy binary has a nonempty frozen cache that stays
unchanged during timing. Its JSON records individual samples, paired ratios,
fixture and executable hashes, adjacent `python314.dll` hashes when present,
version output, and relevant environment settings. Binary hashes are checked
again after the measurements. Generated data and traces stay under `target/`.

For example, from the repository root:

```sh
python3 tools/bench_cold_jit.py \
  --base target/performance/weavepy-reference \
  --new target/release/weavepy \
  --python python3.14 \
  --out target/performance/cold-jit-local/report.json
```

Use a fresh output directory and otherwise idle hardware. Warmed-function
results include an earlier call in their process elapsed time. They don't
replace cold-workload measurements or remove initialization from the gate.
The diagnostic doesn't measure process CPU or RSS.

On Windows PR jobs, the diagnostic runs only after the benchmark gate fails.
Its JSON and raw traces are uploaded separately. The gate's fixtures, work
sizes, thresholds, retry behavior, and pass/fail result remain unchanged.
This tooling change doesn't fix the outstanding runtime regressions.

Local validation used the same accepted executable on both sides, all three
fixtures, both timing modes, and CPython. Six compilation traces retained all
phase markers. Those smoke-test timings aren't optimization evidence. Five
regressions cover DLL identity, actual execution of both calls, child failures,
inherited instrumentation, and empty-cache rejection. Windows execution remains
for CI to validate.
