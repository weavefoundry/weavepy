# WeavePy architecture

This document describes the intended shape of the WeavePy interpreter. It
will outpace the code by a wide margin for a long time; treat it as a
moving target and the place to record design intent before implementation.

## Pipeline

WeavePy is a fairly traditional bytecode interpreter. Source code flows
through the following phases, each owned by a single crate:

```
source ──► lexer ──► parser ──► compiler ──► vm ──► result
            │          │            │         │
       weavepy-       weavepy-    weavepy-   weavepy-
        lexer         parser     compiler     vm
```

- **`weavepy-lexer`** — turns a source buffer into a stream of `Token`s with
  byte spans. Responsible for indentation tracking, implicit line joining
  inside brackets, and f-string lexing (PEP 701). Emits `LexError` on
  malformed input.
- **`weavepy-parser`** — consumes tokens and produces an AST that mirrors
  CPython's `ast` module. The AST module is re-exported from this crate so
  downstream consumers get a single canonical definition.
- **`weavepy-compiler`** — lowers the AST to a `CodeObject` containing
  bytecode instructions, a constant pool, and a names table. The instruction
  set starts as a near-clone of CPython's so we can validate against
  CPython's `dis` output during bring-up; we may diverge later for
  performance.
- **`weavepy-vm`** — owns the runtime: heap, frame stack, builtin types, and
  the dispatch loop. Exposes an `Interpreter` that hosts (REPL, CLI, embeds)
  drive.
- **`weavepy`** — umbrella library that re-exports each pipeline crate and
  offers convenience entry points like `run_source`.
- **`weavepy-cli`** — the `weavepy` binary. Argv-compatible with `python`
  for drop-in use in CI scripts and shebang lines.

## Compatibility strategy

Compatibility with CPython is the primary correctness criterion. The plan:

1. **Track a specific CPython release.** Initially CPython 3.13. Bumping the
   target version is a deliberate, scheduled change, not an accident.
2. **Use the CPython test suite as the acceptance harness.** Successful runs
   of `Lib/test/test_*.py` are the bar for "this feature is done."
3. **Mirror surface APIs aggressively.** AST nodes, `dis` output,
   `sys.implementation`, `__pycache__` layout, exception messages where
   tooling depends on their wording — all of it.
4. **Quarantine intentional divergences.** Any deliberate deviation gets a
   tracking issue, a documented rationale, and a runtime opt-in/out so users
   are never surprised.

## Native extension story (open question)

The single largest open question is how to handle the C-API. Approaches
under consideration:

- **HPy-first.** Encourage migration to the HPy API, where the native
  extension is portable across CPython, PyPy, GraalPy, and now WeavePy.
- **`Py_LIMITED_API` shim.** Implement enough of the limited stable ABI to
  load a useful slice of the ecosystem unmodified.
- **Full `Py_BuildValue`-level CPython API emulation.** Highest compatibility,
  highest engineering cost, and tightly couples our object layout to
  CPython's. Likely the long-tail goal.

A real RFC will land in `docs/rfcs/` before any code commits us to a path.

## Performance roadmap (rough sketch)

Performance work happens after correctness, but the architecture is shaped
with each item in mind from day one:

1. Direct-threaded interpreter loop with computed-goto fallback per target.
2. Inline caches and quickening (à la CPython 3.11+ adaptive specialization).
3. Tiered execution with a baseline and an optimizing tier.
4. Optional JIT (Cranelift first; LLVM-backed tier later if warranted).
5. Aggressive `mmap`-based code-object caching for fast startup.

## Repository conventions

- One crate per pipeline phase. Don't stack unrelated concerns into a single
  crate just because they're small today.
- `unsafe` requires a `// SAFETY:` comment that names the invariant being
  upheld. CI denies missing safety comments via clippy lints.
- Public APIs document their compatibility level: "stable", "tracking
  CPython", or "experimental".
- Snapshot-based tests for parser/compiler output (via `insta`) so reviewer
  diffs are readable.
