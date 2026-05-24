# weavepy-bench

RFC 0021 — `pyperformance`-shaped microbench harness for WeavePy.

The crate is excluded from `default-members` so `cargo build` /
`cargo test --workspace` doesn't pull it in. Opt in with `-p
weavepy-bench` when you want to run the benches.

## Usage

```bash
# Run all fixtures, print a markdown report.
cargo run -p weavepy-bench -- run

# Skip the host CPython subprocess (faster on CI without python3).
cargo run -p weavepy-bench -- run --no-cpython

# Print the report as JSON instead of markdown.
cargo run -p weavepy-bench -- run --json

# Refresh the baseline JSON tracked at `baselines/bench.json`.
cargo run -p weavepy-bench -- run --update-baseline

# Compare current run against the baseline; exit non-zero on
# regression beyond 10% (default threshold).
cargo run -p weavepy-bench -- gate
cargo run -p weavepy-bench -- gate --pct=15
```

Run with `--release` for representative numbers — the dev profile
is far slower than what CI / shipped binaries see.

## Adding a fixture

1. Drop `fixtures/foo.py`. The file should:
   - Import `os`.
   - Define a `bench(n)` callable that runs the workload `n` times.
   - Have a `if __name__ == "__main__":` block that reads
     `WEAVEPY_BENCH_WORK` from the environment so the runner can
     parameterize CPython runs.
2. Add `"foo"` to `FIXTURES` in `src/fixtures.rs`.
3. Pick a default `work` parameter in `default_work(...)`.
4. Run `cargo run -p weavepy-bench -- run --update-baseline` and
   inspect the diff before committing.
