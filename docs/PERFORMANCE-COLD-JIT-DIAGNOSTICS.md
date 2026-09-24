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
inherited instrumentation, and empty-cache rejection. Subsequent Windows
validation is recorded below.


## Windows results at 6dfb4be

The [Windows job](https://github.com/weavefoundry/weavepy/actions/runs/35996783127/job/107623959029)
ran the diagnostic successfully after the unchanged gate failed. The gate
retains sumvm +27.4% and nested_loops +16.3% against the merge base after
retries. Its initial jitloop ratio is 1.134; the deque retry is 1.105. The
initial suite geometric means are 1.017 versus the merge base and 0.89 versus
CPython 3.14.7. Linux and macOS gates pass. These results precede the adjacent
scalar-read change and the regional accumulator experiment.

Seven paired diagnostic cycles give the following candidate/baseline ratios.
Cold is the first workload call; warm is the second call in the same process.
Both calls remain included in the warm process-elapsed measurement.

| Fixture | Cold workload | Warm workload | Cold process elapsed | Warm process elapsed |
| --- | ---: | ---: | ---: | ---: |
| sumvm | 1.265 | 1.011 | 0.901 | 0.912 |
| nested_loops | 1.173 | 1.000 | 0.894 | 0.907 |
| jitloop | 1.122 | 0.973 | 0.908 | 0.917 |

Separate instrumented traces contain seven baseline compilation attempts
before the first workload marker and none in the candidate. The first sumvm
compile takes 0.502 ms in the baseline and 1.201 ms in the candidate; nested
loops take 0.952/1.700 ms. Jitloop's first kernel compilation takes
0.813/1.568 ms, while the later outer compilation takes 0.462/0.471 ms.
These are diagnostic observations from individual traced runs, not additional
timing samples.

The evidence points to cold compiler costs after startup compilation was
deferred: warmed workload execution is approximately unchanged, and process
elapsed time improves even as first-invocation work slows down. It does not
identify the internal compiler stage responsible or establish a fix. Both
metrics matter; precompiling unused functions or moving initialization outside
the timer would not resolve the cost. Raw samples, hashes, and phase-marked
traces remain in `target/performance/ci-6dfb4be-cold-jit/`.
