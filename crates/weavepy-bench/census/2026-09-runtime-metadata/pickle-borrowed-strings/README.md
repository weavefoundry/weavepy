# Reduce pickle allocation and copying

See [REPORT.md](REPORT.md) for gains, regressions, validation, and remaining CPython gaps. Focused comparisons use the bounded-list release 763d9a56; full census and startup comparisons use preceding complete release f4630431. Executables and extracted stdlib copies remain outside the archive. Large pickle inputs and frozen code artifacts are losslessly compressed; compressed-inputs.json records their original identities.
