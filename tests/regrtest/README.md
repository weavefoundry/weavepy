# WeavePy regrtest fixtures

This directory holds small, hand-curated regression tests. Each one
is a complete Python script that:

- exits cleanly when WeavePy is correct, and
- raises an uncaught exception when WeavePy regresses.

The runner in `weavepy-conformance` discovers anything matching
`test_*.py` in this folder, runs it under a fresh interpreter, and
grades the result against `expectations.toml`.

## Adding a test

1. Drop a `test_<area>.py` file in this directory. Use plain
   `assert`s — no third-party test runner is required.
2. Run `cargo run -p weavepy-conformance -- regrtest`. The fresh
   test will show up unlabelled and (assuming it passes) is fine
   without an entry in `expectations.toml`.
3. If the test is expected to fail (e.g. exercises an opcode
   still in flight), add an entry to `expectations.toml` so the
   runner doesn't flag it as a regression.

## Conventions

- Tests should run in well under 1s.
- Tests must not write to disk outside `tempfile`.
- Tests must not require network access.
- Tests must not depend on each other or on test order.

## Status tags

| status     | meaning                                                                       |
|------------|-------------------------------------------------------------------------------|
| `pass`     | exits cleanly                                                                  |
| `fail`     | uncaught exception escaped the script                                          |
| `error`    | parse/compile/IO failure before execution                                      |
| `skip`     | skipped by the expectations file (e.g. needs a missing feature)                |
| `timeout`  | exceeded the per-test wall budget (default 30s, override in `expectations.toml`)|
