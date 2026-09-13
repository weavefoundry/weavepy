# AGENTS.md

Follow Chicago Manual of Style (CMOS) grammar in the documentation, but use straight apostrophes and quotation marks, avoid em dashes, and use contractions where appropriate.

The `weavepy` binary is built by `cargo build --release -p weavepy-cli` (the `weavepy` package is the library only; building it does **not** refresh `target/release/weavepy`).

Keep generated benchmark results, profiles, logs, source snapshots, and research archives under `target/` or in separate research storage. Commit reusable tools, regression fixtures, and concise reports. Required Unicode/codec data, vendored build inputs, and the benchmark baselines used by CI belong in Git. The census directories allow only explicitly selected source and documentation files. Run `python3 tools/check_repository_artifacts.py` before committing; don't use `git add -f` to bypass the artifact policy.

See [Repository artifact policy](docs/REPOSITORY-ARTIFACTS.md) for required-data exceptions and cleanup instructions.
