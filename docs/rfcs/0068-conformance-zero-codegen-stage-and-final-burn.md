# RFC 0068: Conformance zero — the codegen-stage surface, tracing exactness, and the final red-row burn

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-08-15
- **Tracking issue**: TBD
- **Builds on**: RFC 0049/0057/0060 (the measured whole-suite baseline
  and the burn-down protocol this wave finishes), RFC 0033 (the
  `cpython_code` codec the codegen-stage surface extends), RFC 0057 WS6
  (the faithful jump-threading/flowgraph port this wave formalizes into
  an IR), RFC 0031 (the tracing hooks whose event streams this wave
  makes exact), RFC 0054 (the rustls `_ssl` whose verifier this wave
  aligns with OpenSSL defaults), RFC 0036/0060 (the regrtest harness
  and honesty rules that grade all of it).

## Summary

After RFC 0060 and the three subsequent perf/ecosystem/distribution
waves, the measured whole-suite baseline stands at **25 `fail` rows and
6 `skip` rows** against the vendored CPython 3.13 `Lib/test` tree, with
every red row carrying a measured reason. This wave is the last burn:
its acceptance bar is **`fail 0, error 0, timeout 0, unexpected 0`** on
the full sweep, with any test that is *provably unsatisfiable* under
WeavePy's object model moved to a new, explicitly-enumerated
`divergence` status (budget: ≤ 1 row) rather than left as an ambient
red. "Drop-in replacement for CPython" stops being a status paragraph
with footnotes and becomes a single measured number.

A fresh full sweep (`--all-cpython --mode subprocess --jobs 8` on the
post-RFC-0067 default-JIT binary, 2026-08-15) confirms the baseline is
current: **550 labels — pass 518 / fail 25 / error 0 / skip 6**, the
red set identical to the checked-in expectations (one
`test_asyncio/test_events` timeout under `-j8` contention re-ran
standalone to a 15s pass). The 25 rows are not 25 independent
problems. They cluster into seven arcs, in dependency order:

1. **The codegen-stage surface (5 rows — the keystone).**
   `test_compiler_codegen` needs `_testinternalcapi.compiler_codegen`
   (the pre-assembly, labeled pseudo-instruction stream for an AST);
   `test_peepholer` (61 tests, E-flood) needs
   `_testinternalcapi.optimize_cfg` (the flowgraph-optimization stage
   over labeled pseudo-instruction sequences) plus CPython's exact
   optimized-CFG shapes; `test_compile`'s residual (43F/2E) is the
   optimizer/branch-elimination introspection cluster plus the
   remaining `TestInstructionSequence` legs; `test_code`'s single F
   asserts CPython's exact except-as cleanup lowering
   (`test_co_positions_artificial_instructions`); `test_dis` trips at
   import on a module-level introspection helper and then needs
   opcode-coverage/table-format parity. All five are the same missing
   artifact: WeavePy compiles AST → its own instruction set and
   re-encodes to CPython's 16-bit form *post-assembly* (RFC 0033),
   so the pre-assembly stage CPython exposes to its tests does not
   exist. The wave builds it: a CPython-shaped **flowgraph IR**
   (labeled pseudo-instructions, located, with synthetic marks) that
   formalizes the RFC 0057 WS6 jump-threading port, a faithful
   `optimize_cfg` over it, and a `compiler_codegen` lowering that
   produces CPython's documented pseudo-instruction stream.
2. **Tracing and monitoring exactness (3 rows + `test_sys` legs).**
   `test_sys_settrace` (49F residual: `except*`-group and finally-path
   line-event granularity, plus 2 comprehension-inlining jump-matrix
   cases), `test_monitoring`, and `test_trace` (CLI legs exiting rc=1).
   These are downstream of the same location/synthetic-mark discipline
   the flowgraph IR carries, which is why they ride arc 1 rather than
   being patched around it. This cluster is what `coverage.py`, `pdb`
   wrappers, and every bytecode-tracing tool actually exercises.
3. **The `_testcapi` fractal and import fixtures (2 rows).**
   `test_capi`'s remaining per-leg attributes (refcount-exactness
   probes, allocator probes, per-API probe legs — the RFC 0060 WS1
   residual), and `test_import`'s 3F/12E: the
   `_testsinglephase`/`_testmultiphase` C fixtures for
   SubinterpImportTests (multi-init extension isolation over the RFC
   0031 sub-interpreters), frozen-module from-import error shape,
   `PycRewritingTests.test_foreign_code`, and the
   script-shadowing-stdlib error messages.
4. **The importlib machinery row (1 row, the largest by test count).**
   `test_importlib`: 1276 run, 205F/478E — full `FileFinder`
   semantics (directory mtime caching, case-sensitivity probes, loader
   details), frozen-module spec semantics (`__spec__.origin`,
   `loader_state`, reload of frozen), and the `importlib.machinery`
   surface the suite's ABC-conformance matrix asserts.
5. **Introspection truthfulness (4 rows).** `test_sys` (15F/8E:
   displayhook/excepthook printing shapes, structseq
   no-instantiation guards on `sys.flags`/`sys.version_info`,
   `_current_frames`, `call_tracing` arity, getframe fixtures,
   `tracebacklimit` rendering, `switchinterval`, io-encoding
   subprocess legs), `test_types` (24F/10E: PEP 604 union runtime
   semantics — hash/`isinstance`/GenericAlias interop —
   SimpleNamespace repr/replace/constructor, mappingproxy
   constructor+methods, coroutine duck-typing wrappers, int/float
   `__format__` locale edges, `test_internal_sizes`), `test_pydoc`
   (first failure `KeyError: '__doc__'`), and `test_inspect` (the
   recorded PEP 646 parser blocker — `def f(*args: *tuple[int,
   ...])` — no longer reproduces; re-measured fresh, the residual is
   builtin `__text_signature__`/`inspect.signature` legs,
   `getfullargspec` over builtin methods, `getsource` on
   lambdas/one-liners, `getcallargs` error taxonomy, and
   descriptor/coroutine-wrapper edges).
6. **The stdlib long tail (8 rows), each with a fresh first-failure:**
   `test_context` (10F/1E of 46: `Token`/`ContextVar` repr shapes —
   `' used '` markers, default-recursion elision), `test_source_encoding`
   (12F/3E of 69: `SyntaxError` message shapes for coding-cookie
   problems, e.g. `encoding problem for '<string>': ascii`),
   `test_email` (policy.utf8 + `EmailMessage.iter_attachments`),
   `test_logging` (residual after the handler tests), `test_pathlib`
   (E/F cluster in `walk`/`glob`), `test_urllib2_localnet` (2E: the
   rustls verifier is *stricter* than OpenSSL's default — it rejects
   the suite's test certificates for a missing authorityKeyIdentifier
   under an RFC 5280 strict check; OpenSSL's default chain build
   accepts them), `test_file_eintr` (IO subprocess exits rc=1), and
   `test_fork1` (child exit code 42 expected — fork+thread interaction
   in the exit path).
7. **Harness truth (2 rows).** `test_regrtest` (31F/9s: the verbatim
   `test.libregrtest` package runs, but legs asserting the
   *multi-process runner internals* — the worker JSON protocol,
   `--fast-ci`/`--slow-ci` flag plumbing, refleak hunting — need
   `weavepy -m test -jN` to actually spawn and speak CPython's worker
   protocol), and `test_marshal` (2F: `InstancingTestCase.testInt` /
   `testFloat` assert that version-2 marshal loads create *more*
   instances, distinguished by `id()` — unsatisfiable while WeavePy's
   unboxed int/float model derives `id()` from the value; this is the
   candidate for the new `divergence` status, with the two test ids
   enumerated).

The wave also audits the six `skip` rows: `test_embed`, `test_getpath`
and `test_multiprocessing_fork` (on macOS) are principled and stay;
`test_locale` ("setlocale in CI sandbox") and `test_pdb` ("requires a
terminal + readline") predate the RFC 0050 locale work and the RFC
0031 pdb/bdb work respectively and get honest re-measurement; and
`test_socket` graduates from "loopback subset" by landing
`sendmsg`/`recvmsg` with ancillary data (`SCM_RIGHTS`, cmsg
scatter/gather, IPv6 cmsg) and `os.sendfile`, with the SCTP legs
skipping through CPython's own platform gates.

As with every wave since RFC 0036, the deliverable is measured: a full
re-baseline sweep, every touched row rewritten from evidence, and
`unexpected 0` — but this time with reds forbidden rather than
reasoned.

## Motivation

1. **The status paragraph still has an asterisk.** The README claims a
   drop-in replacement "with a measured conformance baseline" — which
   is honest, but 25 red rows is a *footnote-shaped* honesty. `fail 0`
   is a different kind of claim: auditable in one line, defensible in
   one sweep. This is the single most direct move toward the user-visible
   goal of the project.
2. **The biggest cluster gates real tools, not just tests.** The
   tracing-exactness arc (rows: `test_sys_settrace`, `test_monitoring`,
   `test_trace`) is precisely the contract `coverage.py`, debuggers,
   and profilers sit on. The codegen-stage arc is what
   bytecode-introspecting libraries (`dis`-based analyzers, codemod
   tooling, `pytest`'s assertion rewriter relatives) observe. These
   flips harden surfaces users hit, not vanity rows.
3. **The keystone is shared.** Five rows (arc 1) and most of arc 2
   fall out of one artifact — the flowgraph IR. Building it once
   retires the two "effectively a spec for a codegen-stage emulation
   layer" deferrals recorded in the RFC 0060 baseline and un-gates
   `test_compile`'s optimizer-introspection matrix.
4. **Stale reds rot.** `test_inspect`'s recorded blocker no longer
   reproduces (the PEP 646 annotation form parses on the current
   binary). A baseline whose reasons drift from reality stops being a
   measurement. Finishing the burn is also re-establishing that every
   row's reason is *currently* true.
5. **Cost of inaction.** Every future wave re-measures the same 25
   rows; every external evaluation re-discovers the same footnotes;
   and the importlib/`_testcapi` E-floods keep suppressing signal from
   ~2,000 individual tests inside partially-red suites.

## CPython reference

- `Python/flowgraph.c` — the CFG (basic blocks of located
  pseudo-instructions), `_PyCfg_OptimizeCodeUnit` and the
  optimization passes `optimize_cfg` exposes: constant folding,
  jump threading, branch elimination, unreachable-block pruning,
  `NOP` removal discipline, and the exact location-propagation rules.
- `Python/compile.c` + `Python/codegen.c` (3.13 split) — the
  codegen stage: AST → labeled pseudo-instruction sequences
  (`compiler_codegen`'s output shape), pseudo-ops (`LOAD_METHOD`,
  `JUMP`, `SETUP_FINALLY`, …) before assembly resolves them.
- `Python/instruction_sequence.c` + `Python/assemble.c` — the
  instruction-sequence object `_testinternalcapi.new_instruction_sequence`
  wraps (RFC 0060 landed the assemble half; this wave completes
  codegen/optimize).
- `Modules/_testinternalcapi.c` — `compiler_codegen(ast, filename,
  optimize, compile_mode)` and `optimize_cfg(instructions, consts,
  nlocals)` signatures and result shapes; the remaining `_testcapi`
  per-leg probe modules under `Modules/_testcapi/*.c`.
- `Modules/_testsinglephase.c`, `Modules/_testmultiphase.c` — the
  import-machinery fixtures for `test_import`'s SubinterpImportTests.
- `Lib/importlib/_bootstrap.py` + `_bootstrap_external.py` —
  `FileFinder` (mtime-based directory caching, `_fill_cache`,
  case-sensitivity), `FrozenImporter` spec semantics; graded by
  `Lib/test/test_importlib/`.
- `Python/instrumentation.c` + `Python/ceval.c` — the PEP 669 event
  matrix and the line-event emission rules for `except*` groups and
  `finally` duplication; `Lib/test/test_sys_settrace.py`'s
  `jump`-matrix decorators are the spec for `frame.f_lineno`
  assignment eligibility.
- `Lib/test/libregrtest/worker.py` + `run_workers.py` — the worker
  JSON protocol (`-m test` multi-process mode) `test_regrtest`
  asserts.
- `Python/context.c` — `token_repr`/`contextvar_repr` shapes
  (`<Token used var=...>`, default elision).
- `Parser/pegen_errors.c` + `Parser/tokenizer/` — the
  `SyntaxError: encoding problem for <file>: <codec>` family and
  coding-cookie error taxonomy `test_source_encoding` asserts.
- OpenSSL's default `X509_verify_cert` chain building (no AKI
  requirement for a self-issued root in the trust store) vs rustls'
  `webpki` strict path validation — the `test_urllib2_localnet`
  divergence; also `Lib/test/certdata/` regeneration notes.
- `Modules/socketmodule.c` — `sendmsg`/`recvmsg`/`recvmsg_into`,
  `CMSG_LEN`/`CMSG_SPACE`, `SCM_RIGHTS` fd passing; `os.sendfile`.
- `Python/marshal.c` `r_object` instance-creation behavior for
  `TYPE_INT`/`TYPE_BINARY_FLOAT` under version 2 (no interning) — the
  `id()`-distinguishability assumption `test_marshal`'s
  InstancingTestCase encodes, and which an unboxed value model cannot
  satisfy (the proposed `divergence` row).
- Acceptance suites: every row named in the Summary, graded by
  `tests/regrtest/expectations.toml` under the RFC 0049 protocol.

## Detailed design

### WS1 — the flowgraph IR and the codegen-stage surface (keystone)

Formalize what RFC 0057 WS6 built ad-hoc (CPython-faithful jump
threading, synthetic-jump marking, located `NOP` lowering) into an
explicit IR in `weavepy-compiler`:

- **`PseudoInst` / `FlowGraph`**: labeled basic blocks of located
  pseudo-instructions in CPython 3.13's pseudo-op vocabulary, with
  the `NO_LOCATION`/synthetic discipline as first-class data rather
  than flags recovered post-hoc.
- **`codegen`**: a lowering from WeavePy's AST to the pseudo-op
  stream with CPython's exact shapes (the suites are the spec:
  `test_compiler_codegen`'s expected streams, `test_compile`'s
  branch-elimination introspection). WeavePy's *real* pipeline adopts
  this stage — the existing AST → instruction lowering becomes
  pseudo-op lowering + assembly, so the compatibility view and the
  production compiler cannot drift. This is the honest version of an
  "emulation layer": there is one compiler, and its intermediate
  stage is CPython-shaped.
- **`optimize_cfg`**: a faithful port of CPython's flowgraph passes
  (constant folding incl. the 3.13 tuple-of-constants and
  frozenset-in rules, branch elimination, jump threading — already
  ported — unreachable pruning, NOP elision rules, location
  propagation). Exposed via `_testinternalcapi.optimize_cfg` over the
  RFC 0060 instruction-sequence object.
- **`_testinternalcapi.compiler_codegen`**: AST-or-source in,
  labeled pseudo-instruction list out, matching CPython's tuple
  shapes.
- **Except-codegen surgery** (`test_code`'s
  `test_co_positions_artificial_instructions`): adopt CPython's
  except-as cleanup lowering — located fallthrough unbind via the
  flowgraph single-predecessor location copy; the artificial
  exceptional-path duplicate ending `RERAISE 1` with the
  `COPY 3 / POP_EXCEPT / RERAISE 1` tail — so artificial-instruction
  positions match exactly.
- **`test_dis` burn**: fix the import-time helper (a module-level
  `dis.dis()` call currently returns an int where a code object is
  expected), then drive the opcode-coverage/table-format residual to
  zero on the back of the IR (dis renders the same pseudo-op
  metadata CPython's does).

Targets flipped: `test_compiler_codegen`, `test_peepholer`,
`test_compile`, `test_code`, `test_dis`.

### WS2 — tracing, monitoring, and trace-CLI exactness

On top of WS1's location discipline:

- **`test_sys_settrace`'s 49F**: `except*` group unwind line events
  (each `except*` clause match/re-raise emits CPython's exact line
  sequence), `finally`-path duplication granularity (the duplicated
  finally body carries the original line numbers; jumps between the
  duplicates are synthetic), and the 2 comprehension-inlining
  jump-matrix cases (`frame.f_lineno` assignment eligibility into/out
  of inlined-comprehension ranges).
- **`test_monitoring`**: the residual PEP 669 matrix (branch/jump
  events consistent with the new flowgraph shapes; `STOP_ITERATION`
  and re-raise event ordering).
- **`test_trace`**: the `trace` module's CLI legs (`--count`,
  `--module`, listfuncs/trackcalls output shapes) — measured first,
  likely riding the settrace fixes plus output-format parity.
- **`test_sys` tracing legs**: `sys.call_tracing` arity/validation,
  `_current_frames`, getframe fixtures.

Targets flipped: `test_sys_settrace`, `test_monitoring`,
`test_trace` (and part of `test_sys`).

### WS3 — the `_testcapi` residual and import fixtures

- **`test_capi`**: land the remaining per-leg fixture attributes.
  Refcount-exactness legs get real answers where WeavePy's `Arc`
  model produces CPython-compatible observable counts, and CPython's
  own `@support.refcount_test`-style gates where it cannot (those
  gates exist precisely because refcounts are
  implementation-detail); allocator probes report WeavePy's true
  allocator domains; per-API probes route into the real C-API
  implementations. Any leg with no public contract and no
  CPython-provided gate is a candidate for the suite's own skip
  decorators — not a WeavePy-side red.
- **`test_import`**: `_testsinglephase` + `_testmultiphase` as real
  C fixtures in `tests/capi_ext/` (multi-init isolation over RFC
  0031 sub-interpreters — single-phase modules refuse re-init in a
  sub-interpreter exactly as CPython 3.13 does), frozen from-import
  error shape, `PycRewritingTests.test_foreign_code` (executing a
  code object whose `co_filename` is rewritten), and the
  script-shadowing-stdlib error-message parity.

Targets flipped: `test_capi`, `test_import`.

### WS4 — the importlib machinery burn

The single largest row by test count (1276 run, 205F/478E). Measured
clusters, burned in order:

- **`FileFinder` semantics**: `_fill_cache` directory snapshots,
  mtime-based invalidation, `path_importer_cache` interaction,
  case-sensitivity probes (`_relax_case`), and
  `FileFinder.path_hook` closure behavior.
- **Frozen module specs**: `FrozenImporter.find_spec` with
  `loader_state` (origname/filename), frozen package `__path__`,
  reload semantics.
- **ABC conformance matrix**: `importlib.abc` registrations and the
  inspect-based loader/finder API assertions.
- **Extension-loader edges**: the RFC 0060-enumerated residual.

Target flipped: `test_importlib` (with `test_import` reinforcement
from WS3).

### WS5 — introspection truthfulness

- **`test_sys`**: displayhook/excepthook printing shapes, structseq
  no-instantiation guards (`type(sys.flags)()` raises), tracebacklimit
  rendering, `switchinterval` get/set semantics, io-encoding
  subprocess legs (`PYTHONIOENCODING` handling).
- **`test_types`**: PEP 604 `X | Y` runtime semantics
  (`hash(int | str) == hash(typing.Union[int, str])`,
  `isinstance`/`issubclass` dispatch, `types.UnionType` ↔
  GenericAlias interop), SimpleNamespace constructor/repr/`replace`,
  mappingproxy constructor + full method surface, coroutine
  duck-typing wrappers (`types.coroutine`), int/float `__format__`
  locale ('n') edges. `test_internal_sizes` legs that assert CPython
  struct sizes gate under the suite's own `cpython_only` decorators
  where present; any leg that does not is graded honestly (candidate
  divergence only if truly unsatisfiable).
- **`test_pydoc`**: first failure `KeyError: '__doc__'` (a class
  namespace missing `__doc__` where CPython materializes it), then
  the doc-rendering residual RFC 0060 enumerated.
- **`test_inspect`** (re-measured fresh this wave): builtin
  `__text_signature__` coverage feeding `inspect.signature` (the
  `[BuiltinMethodType]`/`[MethodDescriptorType]`/… matrix and
  `test_base_class_have_text_signature`), `getfullargspec` over
  builtin methods (rides the RFC 0057 descriptor registry),
  `getsource`/`findsource` position fidelity for lambdas and
  multiline one-liners, `getcallargs` error-message taxonomy,
  `getmembers` over descriptors, and coroutine-wrapper state.

Targets flipped: `test_sys`, `test_types`, `test_pydoc`,
`test_inspect`.

### WS6 — the stdlib long tail

Measured-first burns, each small and enumerated:

- **`test_context`**: `Token.__repr__` (`<Token used var=...>` with
  the `' used '` marker), `ContextVar.__repr__` default elision and
  recursion shape, plus the 1E.
- **`test_source_encoding`**: the `SyntaxError` taxonomy for
  coding-cookie problems — `encoding problem for '<string>': ascii`
  message prefixes, BOM-vs-cookie conflicts, unknown-encoding
  wording, and the `-c`/string-input variants.
- **`test_email`**: `policy.utf8` (RFC 6532 message/global
  serialization) and `EmailMessage.iter_attachments` semantics.
- **`test_logging`**: the residual after the handler tests
  (measured fresh; the RFC 0060 note points at the SyncManager
  multiprocessing leg — may be a budget row, graded honestly).
- **`test_pathlib`**: the `walk`/`glob` E/F cluster (recursive glob
  symlink discipline, `walk_up` relative-to semantics).
- **`test_urllib2_localnet`**: align the rustls verifier's
  acceptance with OpenSSL's default chain building for the suite's
  certificates (accept a trust-anchor match without
  authorityKeyIdentifier; strictness stays available behind
  `VERIFY_X509_STRICT`, matching CPython's flag semantics) — an
  engine fix in `_ssl`, not a test accommodation.
- **`test_file_eintr`**: the IO subprocess leg exiting rc=1
  (EINTR-retry discipline in the io stack under signal delivery).
- **`test_fork1`**: child exit path returning 42
  (`os._exit`/interpreter-teardown interaction after fork from a
  threaded parent).

Targets flipped: all eight rows.

### WS7 — harness truth: `-m test` workers and the divergence status

- **`weavepy -m test -jN` for real**: implement the multi-process
  runner path the verbatim `test.libregrtest` drives — spawning
  worker subprocesses, the worker JSON protocol
  (`libregrtest/worker.py`), `--fast-ci`/`--slow-ci` flag plumbing,
  and honest stubs-with-skips only where the contract is
  CPython-build-specific (refleak hunting requires
  `sys.gettotalrefcount`, a debug-build-only API; CPython's own
  release builds skip those legs — WeavePy skips them the same way).
  Flips `test_regrtest`.
- **The `divergence` status**: `tests/regrtest/expectations.toml`
  gains `status = "divergence"` — reserved for rows where a test
  asserts behavior *provably unsatisfiable* under WeavePy's
  documented object model, with mandatory `reason` and enumerated
  test ids, counted separately from `fail` in the sweep summary and
  gated exactly like `pass` rows (a divergence row that starts
  passing or failing differently is `unexpected`). Budget for this
  wave: **≤ 1 row** — `test_marshal`, whose
  `InstancingTestCase.testInt`/`testFloat` assert `id()`-distinct
  instances from version-2 marshal loads, unsatisfiable while unboxed
  ints/floats derive `id()` from value (a deliberate, documented
  design choice from the RFC 0058+ performance arc; abandoning
  unboxing to satisfy two identity tests is rejected).
  `docs/CONFORMANCE.md` documents the category and its bar.

### WS8 — the skip-row audit

- **`test_locale`**: re-measure on the current binary (the recorded
  reason predates RFC 0050's locale work); expected to flip to a
  measured row (pass or an honest residual burn in-wave).
- **`test_pdb`**: re-measure (pdb/bdb landed in RFC 0031; the suite
  is doctest-driven and does not actually require a TTY). Expected to
  become a measured row; burn the residual.
- **`test_socket`**: graduate from the "loopback subset" skip by
  landing `sendmsg`/`recvmsg`/`recvmsg_into` with ancillary data
  (`SCM_RIGHTS` fd passing, cmsg scatter/gather, `CMSG_LEN`/
  `CMSG_SPACE`, IPv6 cmsg) and `os.sendfile`. SCTP legs skip via the
  suite's own `IPPROTO_SCTP` platform gates; the previously-hanging
  `SendmsgUDP6Test.testSendmsgBadArgs` is root-caused as part of the
  work. Target: a measured row, `pass` on the grading host.
- **`test_embed`**, **`test_getpath`**, **`test_multiprocessing_fork`**
  (macOS): stay principled skips — they test CPython's build
  artifacts or reproduce CPython's own platform skip. Their reasons
  are re-verified verbatim.

### WS9 — re-measure, re-baseline, and the claim

Per the RFC 0049 protocol: full sweeps
(`regrtest --all-cpython --mode subprocess --jobs 8`) cross-checked on
the grading host; every touched row rewritten from evidence; bundled
regrtests for every engine fix; the ecosystem lane re-verified
against its baseline (36 pass rows green, offline, with self-tests;
the gevent stretch row stays a measured fail per RFC 0066); the
bench gate re-run against
the committed baseline (this wave must not buy conformance with
performance — the WS1 compiler restructuring is the risk to watch);
README status paragraph and `docs/CONFORMANCE.md` rewritten around
the `fail 0` claim and the `divergence` category.

## Acceptance criteria

1. The final whole-suite sweep grades **`fail 0, error 0, timeout 0,
   unexpected 0`**, with **≤ 1 `divergence` row** (`test_marshal`,
   test ids enumerated) and only the ≤ 4 principled skips
   (`test_embed`, `test_getpath`, `test_multiprocessing_fork`-on-macOS
   at minimum; `test_locale`/`test_pdb`/`test_socket` graduate to
   measured rows).
2. The codegen-stage surface lands:
   `_testinternalcapi.compiler_codegen` + `optimize_cfg` over the
   flowgraph IR, and the production compiler emits through the same
   IR (one pipeline, no drift-prone shadow copy).
   `test_compiler_codegen`, `test_peepholer`, `test_compile`,
   `test_code`, `test_dis` all flip.
3. The tracing cluster flips (`test_sys_settrace`, `test_monitoring`,
   `test_trace`) — line-event streams and `f_lineno` jump eligibility
   match CPython exactly on the suites' matrices.
4. `weavepy -m test -j2` runs a real multi-process worker sweep
   (worker JSON protocol); `test_regrtest` flips.
5. `test_socket` runs as a measured row with
   `sendmsg`/`recvmsg`/`SCM_RIGHTS`/`os.sendfile` landed.
6. Ecosystem lane unregressed: the 36 passing rows stay green
   (offline reproducible, self-tests included). The `gevent`
   stretch row (measured fail, RFC 0066) and the numpy self-test
   row stay out of scope — they are ecosystem-wave-4 headlines, not
   conformance rows.
7. Bench gate green against the committed baseline: the WS1 pipeline
   change lands with **no geomean regression** beyond the gate's
   threshold, and startup stays within its envelope.
8. `cargo fmt` / `clippy -D warnings` / `cargo test --workspace` /
   `regrtest --check` / `ecosystem --check` all green.

## Drawbacks

- **WS1 restructures the production compiler's back end.** Routing
  the real pipeline through the flowgraph IR (rather than bolting on
  a test-only emulation) is the honest design but touches the hot
  path: compile time and the RFC 0067 JIT's view of the instruction
  stream. Mitigations: the IR lowers to the *same* final instruction
  set (assembly output is bit-compatible before/after, enforced by a
  golden test over the corpus), and the bench gate blocks geomean
  regressions.
- **`test_capi` refcount legs may not all be satisfiable.** The
  fallback is CPython's own gates (`refcount_test`, `cpython_only`),
  not forced green — but if a leg has neither a gate nor a
  satisfiable contract, it lands as a second `divergence` candidate
  and the budget conversation reopens. Current evidence says the
  remaining legs are probe-availability, not refcount-exactness
  assertions outside gates.
- **`fail 0` is a treadmill claim.** Every CPython point release
  moves the suite. Mitigation: the claim is pinned to the vendored
  3.13 tree at a recorded commit, as it has been since RFC 0036.
- **Breadth risk**, as with every burn wave — 25 rows across seven
  arcs. Mitigated by the keystone structure (arcs 1–2 are one
  artifact), and by the standing measured-first discipline: any row
  whose burn exceeds its budget gets its honest measured reason and
  the wave's acceptance is renegotiated *explicitly* rather than
  slipped silently.

## Alternatives

- **A test-only codegen-stage emulator** (leave the production
  compiler alone; synthesize CPython-shaped streams just for
  `_testinternalcapi`): rejected — two compilers drift, and the
  emulator would need to be a full faithful codegen anyway to satisfy
  `test_peepholer`'s optimized-CFG assertions. If it must be
  faithful, it should be the real one.
- **Skip the codegen rows as "CPython internals"**: rejected —
  `test_compile`, `test_dis`, and the tracing matrices are
  public-contract surface (PEP 626/657/669), and the cluster is the
  majority of the remaining reds.
- **Leave `test_marshal` as a reasoned `fail`**: rejected — "fail 0
  except the ones with good reasons" is the status quo this wave
  exists to retire. A distinct, gated, budgeted `divergence` status
  makes the object-model choice explicit and keeps `fail` meaning
  "work remains".
- **Box ints/floats to satisfy the marshal identity legs**: rejected
  — reverses the performance arc's foundational representation choice
  for two tests that assert an implementation detail CPython itself
  does not document as a contract.
- **Defer `test_socket` again**: rejected — `SCM_RIGHTS` fd passing
  is real engine surface (multiprocessing pickling of sockets, gevent
  and uvloop idioms), not test support; and it is the last `skip` row
  hiding unimplemented functionality.

## Prior art

- **CPython 3.12/3.13** split its compiler into codegen → flowgraph →
  assemble precisely so the stages could be tested in isolation
  (`test_compiler_codegen`/`test_peepholer` are that project's
  artifacts); WS1 adopts the same architecture for the same reason.
- **PyPy** maintains a "differences from CPython" document for
  object-model divergences (id() of small ints/strings among them) —
  the `divergence` status is the measured, per-test version of that
  document.
- **GraalPy** ships CPython's test suite with per-test tag files
  distinguishing "fails" from "won't fix by design", which is the
  same fail/divergence split this RFC formalizes.
- **RFC 0057 WS6** already proved the "port CPython's flowgraph rule,
  don't approximate it" approach on jump threading — the trace-event
  matrices only stabilized once the rule was verbatim.

## Unresolved questions

- Whether `optimize_cfg`'s exact optimized shapes can be satisfied
  while WeavePy's *final* instruction set differs post-assembly in
  places (the IR is CPython-shaped; assembly is WeavePy-shaped). The
  suites grade the IR stage, so this should hold; the golden-output
  test will prove it early in the wave.
- Whether `test_logging`'s residual is semantics or a budget row
  under `-j8` contention (the SyncManager leg) — measured in week 1;
  if budget, the row gets a measured `timeout_seconds` raise, not a
  skip.
- Whether `test_fork1`'s exit-code leg is satisfiable on macOS at all
  (fork-from-threaded-parent teardown is platform-fraught; CPython
  passes it on the grading host, so the default assumption is yes).
- The exact `divergence` grading semantics for *partial* rows: this
  wave's design deselects the enumerated test ids and requires the
  rest of the file green (deselection list in the expectations row),
  vs. grading the whole file. Proposed: enumerated-deselection, so
  the other 64 marshal tests keep their teeth.

## Future work

- The performance wave 6 follow-up (geomean ≤ 1.0× CPython:
  method-call lanes, generator frames in tier 2, kwargs/defaults call
  shapes, float unboxing in loops) — explicitly *not* this wave, but
  WS1's IR gives the JIT a better-structured input for it.
- CPython 3.13.x point-release tracking (re-vendor, re-sweep,
  re-baseline) as a recurring maintenance protocol now that the
  number to defend is `fail 0`.
- The Linux and Windows grading hosts inheriting the `fail 0` bar
  (per-OS rows exist since RFC 0062; the Windows lanes are still
  advisory per RFC 0063/0064).
- 3.14 gap analysis once the 3.13 surface is at zero.
