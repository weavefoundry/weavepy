# Conformance corpus (in-tree)

Small Python fixtures used by `weavepy-conformance` to grade WeavePy against
CPython on every commit. Each file isolates one lexical or syntactic
feature so a regression points at a specific cause.

Files in this directory are graded by every phase of the harness; the
optional `vendor/cpython/Lib/test/` submodule provides a much larger
corpus when present (see `docs/CONFORMANCE.md`).

## Conventions

- File names are `<phase>_<feature>.py`, e.g. `lex_integers.py`.
- Keep each fixture small and focused. If you want to test a combination
  of features, add a new file rather than mixing them.
- Fixtures must be valid CPython 3.13 source — the oracle's failure is a
  bug in the corpus, not a bug in WeavePy.
