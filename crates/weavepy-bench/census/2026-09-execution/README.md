# Historical performance tools

Reusable measurement scripts from the `33c211b7` performance pass remain here.
The [raw results and original environment](https://github.com/weavefoundry/weavepy/tree/33c211b716dc54606841e5911d7298f762c5d2ce/crates/weavepy-bench/census/2026-09-execution/)
are preserved in that commit. The cleanup leaves existing local results intact
but removes them from the current tracked tree.

Run the tools from the repository root and keep new results under `target/`.
The older startup and microbenchmark scripts run immediately, require a saved
`target/release/weavepy-perf-base`, and write to the already ignored
`tmp/performance-20260908/`. The execution probes accept explicit output paths.
See the [artifact policy](../../../../docs/REPOSITORY-ARTIFACTS.md) before adding
new files to this directory.
