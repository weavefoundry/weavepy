# Collections and datetime performance tools

Deque and datetime probes, seeded CPython arithmetic oracles, and compatibility validation.

Run these tools from the repository root. Build the executable with
`cargo build --release -p weavepy-cli`; building the library package alone does
not refresh it. Probe and oracle drivers expose their inputs through `--help`.
The older JSON and collections validation scripts run immediately and do not
support `--help`. Validation drivers require their referenced predecessor and
conformance executables, fixtures, and CPython installation.

Store generated results under `target/`. This directory tracks reusable source
and required test configuration only. Local raw results, logs, snapshots,
reports, and archives are ignored and have not been deleted by the PR cleanup.

The [September 13 checkpoint](https://github.com/weavefoundry/weavepy/tree/c410f1ae7e157f9d53798af4180a77c8f385f2a5/crates/weavepy-bench/census/2026-09-collections-datetime/)
preserves the historical evidence and reports. Its experimental source archives
are separate from the active runtime. The checkpoint documents earlier archive
omissions; it is not a complete backup of every local diagnostic.

See [runtime performance status](../../../../docs/PERFORMANCE-RUNTIME-METADATA.md)
for the current implementation, measured gains, validation, and remaining gaps.
