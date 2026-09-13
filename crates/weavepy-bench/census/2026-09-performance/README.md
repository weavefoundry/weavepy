# Historical performance tools

Reusable measurement scripts from the `971d7521` performance pass remain here.
The [raw results and original environment](https://github.com/weavefoundry/weavepy/tree/971d75214077c07935710798b705ec50d5831860/crates/weavepy-bench/census/2026-09-performance/)
are preserved in that commit. The cleanup leaves existing local results intact
but removes them from the current tracked tree.

Run the tools from the repository root and keep new results under `target/`.
The older startup and microbenchmark scripts run immediately, require a saved
`target/release/weavepy-perf-base`, and write to the already ignored
`tmp/performance-20260908/`. The execution probes accept explicit output paths.
See the [artifact policy](../../../../docs/REPOSITORY-ARTIFACTS.md) before adding
new files to this directory.
