# Benchmark tools and research output

This tree keeps explicitly selected measurement tools and documentation.
Generated results, profiles, snapshots, and archives stay under `target/` or in
separate research storage. New census directories are ignored until their source
files are deliberately selected. See the
[repository artifact policy](../../../docs/REPOSITORY-ARTIFACTS.md).

The historical result collections introduced by `971d7521` and `33c211b7`, and
the earlier wave 12 profiles, remain in their original commits. Current
performance documentation links to those records. Existing local copies remain
available but are no longer tracked.

The baseline in `../baselines/` remains tracked because the benchmark gate uses
it. This cleanup doesn't change benchmark workloads, thresholds, or runtime code.
