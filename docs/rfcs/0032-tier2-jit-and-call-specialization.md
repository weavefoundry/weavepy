# RFC 0032: Tier-2 — a Cranelift JIT for hot numeric frames + CALL specialization

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-29
- **Tracking issue**: TBD
- **Builds on**: RFC 0021 (adaptive specialization / inline caches),
  RFC 0024/0025 (GIL + cross-thread heap), RFC 0031 (observability hot path)

## Summary

RFC 0021 shipped the "tier-1 baseline": per-instruction inline caches and
PEP 659-style adaptive specialization for the seven hottest opcodes. It
**deliberately deferred two things** and named them the next perf RFC:

> - **`CALL` specialization.** The single largest remaining opcode-level
>   perf gap.
> - **Tier-2: Cranelift JIT.** "Once the adaptive interpreter is recording
>   stable type observations, a tier-2 JIT can compile hot frames to
>   native code … this RFC builds the data-collection layer they need."

RFC 0032 cashes both checks. After it lands:

- The `CALL` opcode gains **five inline-cache fast paths** in the
  interpreter — `CallPyExact`, `CallPyExactNoFree`, `CallBuiltinFast`,
  `CallBoundMethodExact`, and `CallTypeConstructor1` — that skip the
  ~120-arm `Interpreter::call` dispatch chain and the elaborate
  `call_python` argument-binding loop when the call shape is simple and
  stable. This is pure interpreter work, always on, and warms through
  the same `Empty → Specialized → Cooldown` cycle as every other RFC
  0021 cache.

- A new **`weavepy-jit`** crate hosts a **tier-2 method JIT** backed by
  **Cranelift** (`cranelift-jit` + `cranelift-frontend` +
  `cranelift-codegen` + `cranelift-module`). The JIT compiles a code
  object's **unboxed numeric/control-flow core** to native machine code:
  `LOAD_FAST` / `STORE_FAST` / `LOAD_CONST` of `int` / `float` / `bool`,
  `BINARY_OP` / `COMPARE_OP` / `UNARY_OP` on `int` / `float`, and the
  conditional and unconditional jumps (`POP_JUMP_IF_*`, `JUMP_FORWARD`,
  `JUMP_BACKWARD`) plus `RETURN_VALUE`. The headline case is the
  **`while`-style integer/float loop**, which lowers to this subset with
  no iterator protocol. `for … range(…)` loops are *not* in the v1
  subset: they compile to a `CALL range` + `GET_ITER` + `FOR_ITER`
  iterator dance that needs an OSR-with-iterator-state path (future
  work). Frames whose hot region steps outside the subset are left to
  the interpreter — the JIT never emits native code for an operation
  whose semantics it can't reproduce exactly.

- The VM gains a **per-`CodeObject` hot counter** (the tiering trigger
  RFC 0021 said the JIT would need but didn't build). Frame entry and
  every `JUMP_BACKWARD` back-edge bump it; when it crosses
  `JIT_HOT_THRESHOLD`, the frame is handed to the JIT compiler once. The
  result is cached on the code object (keyed by `Arc` identity) as
  `Compiled(fn)` or `NotJitable` so we never re-attempt a frame we've
  already rejected.

- **Guards and deopt.** A compiled frame is entered only after an **entry
  guard** confirms the participating locals hold the expected unboxed
  types. Inside native code, integer arithmetic uses **checked** ops:
  on i64 overflow — or any other condition the fast path can't handle —
  the native function takes a **side exit**, writes the live register
  state back into the frame's locals, and returns a `Deopt { pc }`
  status so the interpreter resumes at exactly that bytecode offset with
  identical state. Deopt is always semantically transparent: the JIT is
  a pure accelerator, never a source of observable behavior change.

- **On-stack replacement (OSR)** is designed-for but **deferred** in
  v1: the hot counter fires on back-edges, but the JIT enters only at
  the function start (pc = 0), so a function must be *re-entered* (called
  again) to run native — which covers the common "hot helper called in a
  loop / repeatedly" case and the bench harness. Lifting an
  already-running loop mid-flight (true OSR) needs the multi-entry
  machinery sketched below and lands in a follow-up.

- The JIT is **off by default** and gated three ways: the `jit` Cargo
  feature on `weavepy-vm` / `weavepy-cli` / `weavepy-bench` (built by
  CI's `--all-features`, absent from a plain `cargo build`), and the
  `WEAVEPY_JIT=1` environment variable (or `-X jit`) at runtime. With
  the feature off the VM compiles a zero-cost no-op shim; with the
  feature on but the env var unset, the hot counter still ticks but the
  compiler is never invoked.

- The **bench harness** learns to capture the host-CPython baseline
  (the existing `bench.json` has `"cpython": null` because runs passed
  `--no-cpython`) and to run WeavePy in three modes — interpreter,
  tier-1 (specialized), and tier-2 (JIT) — so the speedup of each tier
  is a tracked, regression-gated number. `WEAVEPY_VM_STATS` grows JIT
  counters (frames compiled, native entries, deopts, bailouts).

Net diff: **~22–30K LOC** (the `weavepy-jit` crate, the VM integration
and CALL specialization, the bench/stat wiring, fixtures, tests, and
this RFC), plus the Cranelift dependency tree.

## Motivation

A drop-in replacement that is correct but 10–50× slower than CPython is
not, in practice, a drop-in replacement — nobody swaps in an interpreter
that turns a 2-second script into a 40-second one. RFC 0020 made every
workflow *work*; RFC 0021 made the dispatch loop *competitive* with a
naive switch; but the project's stated goal #2 ("Performance second, but
seriously … tiered execution, inline caches, specialization, and a JIT
are all on the long-term roadmap") still had two unchecked boxes, and
they are the two that matter most for hot code:

1. **Calls dominate real Python.** Every method call, every helper, every
   recursion step goes through `OpCode::Call → Interpreter::call →
   call_python`. `call()` is a ~120-arm `if b.name == "..."` ladder for
   builtins plus a match over callable kinds; `call_python` rebuilds a
   `Vec<Object>` of locals, runs a keyword-binding loop, applies
   defaults, and constructs a `Frame` — on *every* call, even
   `f(x)` where `f` is a plain two-arg Python function called a million
   times. CPython specializes exactly this (`CALL_PY_EXACT_ARGS`,
   `CALL_BUILTIN_FAST`, `CALL_BOUND_METHOD_EXACT_ARGS`, …); we deferred
   it in RFC 0021 to keep that RFC reviewable. It is the cheapest large
   win left in the interpreter.

2. **Hot numeric loops want native code.** `fib`, `nbody`,
   `nested_loops`, and `sumvm` in our own bench suite are tight loops
   over `int` / `float`. The tier-1 specialization removed the
   dunder-search and the dict-keyed lookups, but every iteration still
   pays for: the `match ins.op` dispatch, the `Object` enum tag
   check/clone, the `Vec<Object>` stack push/pop, and the per-opcode
   cache read. A method JIT collapses an entire loop body into a handful
   of machine instructions operating on values in registers. This is
   the difference between "single-digit× slower than CPython" and
   "competitive with or faster than CPython" on numeric kernels.

RFC 0021 explicitly built the data-collection layer the JIT consumes:
the inline caches already record, per call site, which concrete types
flow through each `BINARY_OP` / `COMPARE_OP` / `FOR_ITER`. The JIT reads
those caches to decide what to assume, and emits the matching guards.
The two threads of this RFC are therefore the natural, pre-planned
continuation of 0021 rather than a new direction.

## CPython reference

This RFC tracks **CPython 3.13** for the call-specialization shapes and
the deopt discipline, and borrows the *architecture* (not the
implementation) of tiered JITs from the wider ecosystem.

- **`Python/specialize.c` / `Python/bytecodes.c`** — the
  `CALL_PY_EXACT_ARGS`, `CALL_BOUND_METHOD_EXACT_ARGS`,
  `CALL_BUILTIN_FAST`, `CALL_TYPE_1` specialized opcodes and their
  guards (function-version check, arg-count match, no-kwargs, builtin
  flags). Our five fast paths mirror that set.
- **PEP 659** — the warm-up / fingerprint-guard / deopt model RFC 0021
  adopted; CALL specialization reuses it verbatim.
- **CPython 3.13's experimental tier-2 / "copy-and-patch" JIT
  (`Tools/jit/`)** — informal reference for the *idea* of compiling hot
  micro-ops to native code with deopt side exits. We do not adopt
  copy-and-patch; we use Cranelift as a real optimizing backend, which
  is closer in spirit to:
- **PyPy's meta-tracing JIT** and **Cinder's HIR/LIR method JIT** — for
  the guard/deopt/OSR discipline: a compiled trace is valid only while
  its type assumptions hold, and any violation transfers control back to
  the interpreter at a well-defined bytecode boundary with reconstructed
  state.
- **Cranelift** (`cranelift-jit`, as used by Wasmtime) — the codegen
  backend. Chosen over LLVM for a far smaller blast radius, fast
  compile times suitable for a JIT (not an AOT compiler), pure-Rust
  build, and proven cross-platform support (x86-64 + aarch64 on Linux /
  macOS / Windows).

We deliberately do **not**:

- JIT the full opcode set. Containers, attribute access, calls into
  Python/builtins, exceptions, generators, and the import machinery stay
  in the interpreter. The JIT is a *numeric-core accelerator*, not a
  whole-language compiler. (Calls *out* of a JITed frame deopt; calls
  are accelerated by the tier-1 CALL specialization instead.)
- Promote `int` past `i64`. The unboxed integer path is `i64`; overflow
  deopts to the interpreter, which constructs `Object::Long`. This
  matches the bet RFC 0021's `BinOpAddInt` fast path already makes.
- Persist compiled code across runs. The JIT cache is per-process, like
  the inline caches (and like CPython's).
- Implement register allocation or instruction selection ourselves —
  Cranelift owns that.

## Detailed design

### Part A — `CALL` specialization (interpreter, tier-1)

#### New `InlineCache` variants

`weavepy-compiler/src/bytecode.rs` grows five variants on the existing
`InlineCache` enum (still `Copy`, still ≤ 24 bytes):

```rust
pub enum InlineCache {
    // ... existing RFC 0021 variants ...

    // CALL family (RFC 0032).
    /// Callable is a specific `PyFunction`; arg count matches exactly;
    /// no *args/**kwargs/defaults/kwonly needed; the function has no
    /// free variables (closure empty) so the frame needs no cells.
    CallPyExactNoFree { func_id: u64, argc: u32 },
    /// Same, but the function carries a closure; the fast path still
    /// skips arg-binding but builds cells.
    CallPyExact { func_id: u64, argc: u32 },
    /// Callable is a specific Rust builtin known to be pure w.r.t. the
    /// call protocol (no kwargs handling needed); skip the name ladder.
    CallBuiltinFast { builtin_id: u64, argc: u32 },
    /// Callable is a bound method whose function is a `PyFunction` with
    /// an exact-arity body; prepend `self` and dispatch as `CallPyExact`.
    CallBoundMethodExact { func_id: u64, argc: u32 },
    /// Callable is a type with a one-argument constructor fast path
    /// (`int`/`float`/`str`/`bool`/`list`/`tuple` of one arg).
    CallTypeConstructor1 { type_id: u64 },
}
```

`func_id` / `builtin_id` / `type_id` are `Rc::as_ptr(...) as u64`
fingerprints, identical in spirit to RFC 0021's `type_id`. The guard
re-checks the fingerprint on every dispatch; a miss deopts to
`Cooldown(COOLDOWN)` exactly as the existing caches do.

#### The fast paths

In `Interpreter::step`, the `OpCode::Call` arm is restructured to mirror
`BINARY_OP`:

```rust
OpCode::Call => {
    match frame.code.caches.get(cache_pc) {
        InlineCache::CallPyExactNoFree { func_id, argc } => {
            if self.try_call_py_exact_nofree(frame, func_id, argc)? { /* done */ }
            else { self.call_generic_and_specialize(frame, ins.arg, cache_pc)?; }
        }
        // ... other variants ...
        InlineCache::Empty => self.call_generic_and_specialize(frame, ins.arg, cache_pc)?,
        InlineCache::Cooldown(n) => { decrement; self.call_generic(frame, ins.arg)?; }
        _ => self.call_generic_and_specialize(frame, ins.arg, cache_pc)?,
    }
}
```

`try_call_py_exact_nofree` is the hot one. Guard: TOS-(argc) is the
cached `Object::Function`, `args.len() == code.arg_count`,
`!has_varargs && !has_varkeywords && code.kwonly_count == 0`,
`code.cellvars.is_empty() && code.freevars.is_empty()`, and the call
site has no keyword args. On a hit it builds the locals `Vec` directly
(positional slid into place, padded with `None`), constructs the
`Frame`, and runs it — skipping the entire keyword/default/`*args`
machinery in `call_python`. The specializer
`call_generic_and_specialize` runs the existing generic `call()` and, if
the observed callable + arg shape matches one of the five patterns,
installs the corresponding cache.

`CallPyExact` is selected when `code.arg_count == argc` but the function
has a closure; it skips arg-binding but still runs `make_frame` for the
cells. `CallBuiltinFast` covers the common arity-checked builtins that
don't need the kwargs branch. `CallBoundMethodExact` handles `x.f(a)`
where `f` resolves to a plain method. `CallTypeConstructor1` covers
`int(x)` / `float(x)` / `len(x)`-shaped one-arg type calls.

Generators / coroutines / async generators are **never** specialized
(their call returns a suspended object, not a frame result) — the guard
checks `!code.is_generator && !code.is_coroutine && !code.is_async_generator`.

### Part B — the tier-2 JIT (`weavepy-jit`)

#### Crate layout

```
crates/weavepy-jit/
├── Cargo.toml            # cranelift-* deps; `default-members`-excluded? no — see gating
├── src/
│   ├── lib.rs            # public API: JitEngine, JitStatus, compile(), enter()
│   ├── analyze.rs        # JITability analysis over a CodeObject
│   ├── ir.rs             # the typed mid-IR (TInstr) the analyzer emits
│   ├── lower.rs          # TInstr -> Cranelift IR (FunctionBuilder)
│   ├── runtime.rs        # the ABI: JitFrame layout, side-exit struct, helpers
│   ├── engine.rs         # JITModule lifecycle, function cache, codegen ctx
│   └── value.rs          # the unboxed value representation + type lattice
└── tests/
    └── numeric.rs        # compile + run numeric kernels, compare to expected
```

`weavepy-jit` depends only on `weavepy-compiler` (for `CodeObject` /
`OpCode` / `Instruction` / `InlineCache`) and the Cranelift crates. It
does **not** depend on `weavepy-vm`, to avoid a cycle: the VM owns the
`Object` model and calls *into* the JIT, passing an erased pointer to
the frame's numeric slots and a couple of callback function pointers for
the rare runtime-assist cases. The JIT speaks only in `i64` / `f64` /
`bool` lanes plus the side-exit protocol.

#### The unboxed value model (`value.rs`)

The JIT reasons about a small type lattice:

```rust
enum JitType { Int, Float, Bool, Unknown }
```

Every operand-stack slot and every participating local is assigned a
`JitType` by abstract interpretation during analysis. Only `Int`
(backed by `i64`), `Float` (`f64`), and `Bool` (`i8`) are
representable; anything that would produce `Unknown` makes the region
non-JITable. Inside Cranelift, `Int`/`Bool` are `types::I64`/`I8` and
`Float` is `types::F64`.

#### JITability analysis (`analyze.rs`)

Given a `CodeObject`, the analyzer walks the instruction stream and
builds a control-flow graph at the bytecode level (basic blocks split at
jump targets and after branches). It then runs a forward abstract
interpretation tracking the `JitType` of every stack slot and local. A
code object is **JITable** iff:

1. Every opcode is in the supported set:
   `Nop`, `Resume`, `LoadConst` (int/float/bool only), `LoadFast`,
   `StoreFast`, `BinaryOp` (Add/Sub/Mult/FloorDiv/Mod/And/Or/Xor on int;
   Add/Sub/Mult/Div on float; true-`Div` on int → float), `CompareOp`,
   `UnaryOp` (Neg/Pos/Not/Invert), `PopJumpIfTrue`, `PopJumpIfFalse`,
   `JumpForward`, `JumpBackward`, `CopyTop`, `Swap`, `PopTop`,
   `ReturnValue`. (`FOR_ITER`/`GET_ITER` and therefore `for … range`
   loops are explicitly out of the v1 subset — see future work.)
2. The abstract interpreter never needs `Unknown` for an operand to a
   supported opcode (e.g. `int + str` is out; the analyzer sees `Str`
   inputs are impossible to represent and bails). Arithmetic/compare
   operands must share a lane (both `int`/`bool` or both `float`);
   mixed `int`/`float` bails, except `int / int` which lowers to a
   dedicated float-producing op.
3. The operand stack is **empty at every basic-block boundary** —
   true for ordinary numeric code, but it rules out short-circuit
   `and`/`or` and `a if c else b` in the hot region (they leave a value
   live across a branch). Those need Cranelift block parameters and are
   future work. Each local slot has a single stable [`JitType`] across
   the region (straight-line retyping bails).

The verdict is recorded so it is computed at most once per code object.
The supported set is intentionally the same family RFC 0021 already
specializes, so a JITed frame's assumptions match the inline-cache
observations.

#### Mid-IR (`ir.rs`)

Rather than emit Cranelift directly from bytecode, the analyzer lowers
the supported opcodes to a tiny typed IR (`TInstr`) over virtual
registers (the abstract stack). This decouples the bytecode quirks
(stack discipline, `arg` packing) from Cranelift emission and keeps
`lower.rs` a straight syntax-directed translation. Example `TInstr`s:
`ConstI64(reg, v)`, `LoadLocalI64(reg, slot)`, `StoreLocalI64(slot,
reg)`, `IAdd(dst, a, b)`, `FCmp(dst, op, a, b)`, `BrIf(reg, then_bb,
else_bb)`, `Br(bb)`, `DeoptIf(cond, pc)`, `Deopt(pc)`, `RetI64(reg)`.

#### Cranelift lowering (`lower.rs`)

Each compiled code object becomes one Cranelift function with the ABI:

```
fn(jit_frame: *mut JitFrame) -> i64   // returns a JitStatus discriminant
```

`JitFrame` (`runtime.rs`) is a `#[repr(C)]` struct the VM fills before
entry and reads after exit:

```rust
#[repr(C)]
pub struct JitFrame {
    /// Pointer to a slab of i64-sized slots, one per local. Ints and
    /// bools live here directly; floats are bit-cast through the same
    /// slot. The VM packs/unpacks against its `Object` locals around
    /// the call.
    pub locals: *mut u64,
    pub n_locals: u32,
    /// On a `Returned` exit: the return value (bit pattern + a tag
    /// the VM uses to rebuild an `Object`).
    pub ret_bits: u64,
    pub ret_tag: u32,
    /// On a `Deopt` exit: the bytecode pc to resume at, plus the live
    /// operand-stack contents (so the interpreter can rebuild its
    /// stack). Stack values are written here top-down.
    pub deopt_pc: u32,
    pub stack_spill: *mut u64,
    pub stack_spill_tags: *mut u32,
    pub stack_len: u32,
}
```

Locals are loaded into Cranelift SSA values at function entry (or at the
OSR entry block), arithmetic is emitted inline, and `STORE_FAST`
writes back to the SSA value (mem write-back happens only on exit). The
function has exactly the basic-block structure the analyzer computed;
back-edges become Cranelift loop back-edges, so Cranelift's own
optimizations (LICM-adjacent, GVN, regalloc) apply.

Integer `BINARY_OP` emits `iadd`/`isub`/`imul` **with an overflow
check** (`iadd_cof` / explicit `icmp` on the carry) and a `DeoptIf` to a
side-exit block on overflow. Float ops emit directly. `COMPARE_OP`
emits `icmp` / `fcmp`. Truth tests for the jumps emit the same
zero/NaN-aware logic the interpreter uses.

#### Guards, side exits, and deopt (`runtime.rs` + VM)

Two guard layers keep the JIT transparent:

1. **Entry guard (VM-side).** Before entering native code, the VM checks
   that every *live-in* local the analysis marked `Int`/`Float`/`Bool`
   actually holds that `Object` variant. If not, it does **not** enter —
   it runs the interpreter for this activation. (Cheap: a handful of
   `matches!` checks, only at the tiering boundary, not per iteration.)

2. **Side exits (native-side).** Conditions that can arise mid-execution
   — i64 overflow, a `range` whose step/stop don't fit the fast path, a
   division by zero, a value that flowed into `Unknown` despite the
   static type (shouldn't happen given the entry guard, but defended) —
   branch to a side-exit block that spills the live SSA registers into
   `JitFrame.stack_spill` (with tags), sets `deopt_pc`, and returns
   `JitStatus::Deopt`. The VM then **rebuilds its operand stack and
   locals from the spill** and resumes interpretation at `deopt_pc`. The
   bytecode offset and stack shape are chosen so resumption is
   bit-for-bit identical to never having entered the JIT.

Division by zero and other *raising* conditions deopt rather than raise
from native code: the interpreter re-executes the offending opcode and
raises the exception through the normal path, so tracebacks, line
numbers, and `sys.settrace` events are unaffected.

#### OSR (on-stack replacement)

When the hot counter fires on a `JUMP_BACKWARD` (a loop back-edge), the
frame is already mid-execution. The compiled function therefore exposes
**multiple entry points**: a normal entry (pc = 0) and one OSR entry per
loop header. The VM, holding a live `Frame`, picks the OSR entry whose
pc matches the back-edge target, packs the current locals into the
`JitFrame`, and calls in. Cranelift models this as a function with an
entry `block` that branches to the requested header based on an
`entry_pc` parameter. If the loop later exits to code outside the
region, the function returns `Returned`/`Deopt` and the interpreter
takes over.

#### The hot counter and tiering trigger (VM)

`CodeObject` (or a side-table keyed by its `Arc` pointer in the VM —
chosen to avoid serializing counters through marshal) carries:

```rust
struct HotState {
    counter: AtomicU32,          // bumped at entry + back-edges
    tier: Cell<JitTier>,         // Cold | Pending | Compiled(fn) | NotJitable
}
```

`JIT_HOT_THRESHOLD` defaults to `~50` (tunable via `WEAVEPY_JIT_THRESHOLD`).
On crossing it the VM calls `weavepy_jit::compile(code, caches)`; the
result installs `Compiled(ptr)` or `NotJitable`. The check is a single
relaxed atomic increment + compare on the back-edge — the same shape as
the existing eval-breaker poll, and only "interesting" on the cold
transition.

#### Gating

- **Cargo feature `jit`** on `weavepy-vm` (re-exported by `weavepy`,
  `weavepy-cli`, `weavepy-bench`). Off by default → a plain
  `cargo build` pulls in **no** Cranelift and the VM's `tier2` module is
  a set of `#[inline] fn … {}` no-ops. CI's `--all-features` turns it on,
  so clippy/test/MSRV all exercise the real path.
- **Runtime env `WEAVEPY_JIT`** — `1`/`on` enables, unset/`0` disables.
  With the feature compiled but the var unset, the hot counter still
  ticks (negligible) but `compile()` is never called. `-X jit` on the
  CLI sets it too.
- **`WEAVEPY_JIT_THRESHOLD`**, **`WEAVEPY_JIT_DUMP`** (dump CLIF/disasm
  for debugging) round out the knobs.

### Part C — bench + stats

- `weavepy-bench` gains a `--jit` flag (run WeavePy with `WEAVEPY_JIT=1`)
  and stops passing `--no-cpython` in the tracked CI run, so `bench.json`
  finally records the host-CPython column and a tier-1-vs-tier-2 ratio.
  A new `report` column shows `interp / jit / cpython` medians and the
  two speedup ratios.
- `WEAVEPY_VM_STATS` grows a JIT block: `frames_seen`, `frames_compiled`,
  `frames_notjitable`, `native_entries`, `osr_entries`, `deopts`,
  `entry_guard_failures`.

## Drawbacks

- **Cranelift is a large dependency.** It adds ~30 transitive crates and
  a few MB to a `--features jit` binary, and bumps the workspace MSRV to
  **1.93** (Cranelift 0.132's floor). We accept this: the feature is
  off by default, the MSRV bump is cheap for an experimental project
  pinned to `stable`, and Cranelift is the same backend Wasmtime ships
  cross-platform.
- **The JITable subset is narrow.** Only unboxed `int`/`float`/`bool`
  numeric/control-flow code compiles in this RFC. A frame with a single
  attribute access or container op anywhere in its hot region stays in
  the interpreter. This is the safe, correct starting point; widening
  the subset (subscript, calls-from-native, list/tuple fast paths) is
  future work.
- **Deopt has a cost.** A frame that compiles and then deopts every
  iteration (e.g. an `int` loop that overflows immediately) is slower
  than the pure interpreter for that activation. The entry guard plus
  the `NotJitable`/cooldown bookkeeping bound the damage, and the hot
  counter ensures we only ever try on genuinely hot frames.
- **More `unsafe`.** Calling a JIT-produced function pointer and the
  `#[repr(C)]` `JitFrame` marshalling are `unsafe` by nature. They are
  confined to `weavepy-jit::engine`/`runtime` and the single VM call
  site, each with a `// SAFETY:` note, per the project's `unsafe` policy.
- **Compile latency.** Cranelift compiles fast (µs–low-ms per function),
  but it is not free; a short-lived script that just crosses the
  threshold pays a compile it barely amortizes. The threshold is tuned
  so this is rare, and the env knob lets users opt out.
- **Two type-feedback sources can disagree.** The inline caches observe
  types at the opcode level; the JIT's static analysis assumes them. If
  they diverge (polymorphic loop), the entry guard or a side exit
  catches it and we fall back. Correctness is never at risk; only the
  speedup is.

## Alternatives

- **A bytecode-trace JIT (PyPy-style meta-tracing).** More powerful for
  polymorphic code, but far larger and harder to make correct; a method
  JIT over a typed subset is the smaller, safer first step.
- **Copy-and-patch (CPython 3.13's tier-2).** Lower compile latency, no
  Cranelift dependency, but requires a build-time stencil generator and
  hand-written templates per micro-op, and produces worse code than a
  real optimizing backend. Cranelift gives us regalloc + opt for free.
- **LLVM (via `inkwell`).** Better codegen, but enormous build/runtime
  footprint and slow compiles — wrong tradeoff for a JIT.
- **Ship CALL specialization only, defer the JIT again.** Half the size,
  ~70% of the interpreter-level win, but leaves the headline "native
  code for hot loops" box unchecked yet again. Since the data layer
  exists and the user asked for the big swing, we land both.
- **Always-on JIT (no feature gate).** Rejected: keeps Cranelift out of
  the default build and out of the regrtest CLI, and lets the
  correctness-critical default path stay Cranelift-free.

## Prior art

- **PyPy** — meta-tracing JIT; the guard/deopt discipline and "the
  interpreter is the source of truth, the JIT is an accelerator that can
  always bail" philosophy.
- **Cinder (Meta)** — HIR/LIR method JIT on top of 3.x specialization;
  closest in shape to what we build (method JIT consuming inline-cache
  type feedback, deopt to the interpreter).
- **CPython 3.13 tier-2 + copy-and-patch JIT** — the micro-op + side-exit
  model; we adopt the *discipline*, not the *mechanism*.
- **GraalPy / Truffle** — partial-evaluation JIT; same "specialize on
  observed types, deopt on violation" idea in a different host.
- **Cranelift / Wasmtime** — the backend and the precedent that
  Cranelift is production-grade for JIT codegen across our target
  platforms.

## Unresolved questions

- **Threshold tuning.** `JIT_HOT_THRESHOLD` and the OSR-vs-next-call
  decision are guesses; the bench harness will inform real values.
- **Float NaN / signed-zero corner cases** in `COMPARE_OP` must match
  the interpreter exactly; covered by differential fixtures but worth
  re-auditing if `test_float` ever joins the regrtest allowlist.
- **`FOR_ITER` over `range` with non-unit / negative step** — included,
  but the boundary conditions (empty range, `range(stop)` vs
  `range(start, stop, step)`) need the same off-by-one care the
  interpreter's `ForIterRange` cache already took.
- **Per-thread JIT cache under free-threading.** Today the JIT cache is
  guarded by the GIL like the inline caches. A future no-GIL build
  (post-RFC) needs per-thread or lock-protected code caches; out of
  scope here.
- **Cache invalidation.** A compiled frame assumes the function bodies
  it does *not* call have not changed its own bytecode. Since we key on
  `Arc<CodeObject>` identity and code objects are immutable once
  compiled, invalidation reduces to "drop the cache entry when the code
  object is dropped," which `Arc` handles.

## Future work

- **Widen the JITable subset:** `BINARY_SUBSCR`/`STORE_SUBSCR` for
  `list`/`tuple`, `LOAD_GLOBAL` of stable builtins, and string ops.
- **Calls from native code:** inline a JITed callee into a JITed caller,
  or emit a fast call-into-interpreter trampoline so a JITed loop with a
  call body doesn't fully deopt.
- **Boxing elision across deopt** so a deopt mid-loop doesn't re-box every
  local.
- **Tier-up heuristics:** recompile with more aggressive assumptions when
  a frame proves monomorphic over many activations.
- **`SEND`/generator JIT** for `asyncio`-heavy code.
- **Persistent code cache** keyed by a code-object content hash.
- **Cranelift `cranelift-jit` → `cranelift-object` AOT mode** for an
  experimental ahead-of-time `weavepy build` someday.

## Implementation status (post-merge)

| area | status | notes |
|------|--------|-------|
| `InlineCache` CALL variants (5) | ✅ done | `CallPyExact[NoFree]`, `CallBuiltinFast`, `CallBoundMethodExact`, `CallTypeConstructor1` |
| `OpCode::Call` fast-path arm + specializer | ✅ done | mirrors the RFC 0021 `BINARY_OP` shape; deopt + cooldown |
| `weavepy-jit` crate (Cranelift) | ✅ done | analyze / ir / lower / engine / runtime / value |
| JITability analysis | ✅ done | CFG + abstract type interpretation over the supported subset |
| Cranelift lowering (numeric core) | ✅ done | int/float/bool arith (incl. floor-div/mod), compare, jumps, return |
| Entry guard + side-exit deopt | ✅ done | overflow / div-zero / type-miss deopt to interpreter at exact pc |
| Per-`CodeObject` hot counter + tier cache | ✅ done | `Cold/Pending/Compiled/NotJitable`; `WEAVEPY_JIT_THRESHOLD` |
| OSR loop entry | 🔜 deferred | v1 enters whole-function at pc=0 (helps re-called hot fns); mid-loop OSR is future work |
| `FOR_ITER` / `for … range` loops | 🔜 deferred | needs OSR-with-iterator-state; `while` loops cover the v1 numeric case |
| `jit` Cargo feature + `WEAVEPY_JIT` gate | ✅ done | off by default; CI `--all-features` exercises it |
| Bench: CPython baseline + `--jit` tier column | ✅ done | `bench.json` records cpython + tier-1/tier-2 ratios |
| `WEAVEPY_VM_STATS` JIT counters | ✅ done | compiled / native-entries / deopts / guard-failures |
| Differential regrtest fixtures | ✅ done | numeric kernels equal under interp and JIT; deopt/OSR/CALL paths |
| MSRV bump 1.85 → 1.93 | ✅ done | Cranelift 0.132 floor |
| Widen JITable subset (subscr/calls) | 🔜 deferred | future-work section |
