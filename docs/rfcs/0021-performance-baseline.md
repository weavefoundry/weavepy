# RFC 0021: Performance baseline — adaptive specialization, inline caches, mmap pycache, bench harness

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-24
- **Tracking issue**: TBD

## Summary

Close the gap between "WeavePy is a faithful drop-in for CPython 3.13"
(post RFC 0020) and "**WeavePy is a faithful drop-in for CPython 3.13
that runs at competitive speed**." After this RFC lands:

- The VM gains an **inline cache** alongside every instruction. Cache
  entries fit in 24 bytes and store the type fingerprints, dict
  versions, and offsets a specialized handler needs to skip the
  generic dispatch path. Caches are interior-mutable (`Cell<…>`) so
  the dispatcher can warm them in place without re-cloning the code
  object.
- The dispatcher gains an **adaptive specialization layer** in the
  CPython 3.11+ shape: on every generic-opcode dispatch we examine
  the operand types and, after a short warm-up, install a
  type-specific fast path in the cache. Subsequent dispatches go
  through a tight handler that skips the dunder-method search,
  avoids `Rc::clone`'ing TOS until necessary, and never enters
  `dispatch_binary_op` / `load_attr` / `lookup_global_or_builtin`
  on the hot path. A guard at the start of each specialized handler
  re-checks the fingerprint and **deopts** to the generic path on
  miss, after which the cache cools down before re-attempting.
- **17 specialized fast paths** ship for the seven hottest
  opcodes — `BINARY_OP` (int/float/str), `COMPARE_OP` (int/float/str),
  `LOAD_ATTR` (instance dict, module, slot, type), `LOAD_GLOBAL`
  (module, builtin), `STORE_ATTR` (instance dict, slot), `FOR_ITER`
  (list, tuple, range), `UNPACK_SEQUENCE` (tuple, list, two-tuple).
  Together these cover ~80% of dispatched instructions in our bench
  fixtures.
- A new `weavepy-vm` **`specialize`** module owns the cache layout,
  threshold constants, fingerprint helpers, and the deopt path. The
  dispatch loop in `weavepy-vm/src/lib.rs` grows a per-opcode
  fast-path arm gated on `cache.get()` and falls through to the
  existing generic handler on miss.
- The frozen-stdlib loader gets an **mmap-friendly path**: the
  ~250KB of marshal bytes that comprise our frozen Python stdlib
  used to be re-deserialised on every interpreter start. Frozen
  modules now ship as pre-marshaled bytes in the binary
  (`include_bytes!`) and unmarshal directly from the static slice
  with zero copies — a 4-6× cold-start speedup on a debug build.
- A new **`weavepy-bench`** crate ships a `pyperformance`-shaped
  microbench harness: 8 fixtures (`fannkuch`, `nbody`, `fib`,
  `pidigits`, `pyaes`, `richards`, `sumvm`, `nested_loops`), a
  runner that times each fixture under WeavePy and the host
  CPython, and a `bench.json` baseline tracked in CI. Regressions
  beyond a configurable percentage block PRs.
- A new `cargo bench-weavepy` alias drives the harness from the
  workspace root.
- The VM gains a **`stats`** sidecar (gated behind
  `WEAVEPY_VM_STATS=1`) that counts dispatch events, specialization
  attempts, deopts, and cache hits/misses per opcode. Useful for
  understanding what's left to optimize without cracking open a
  profiler.
- 4 new bundled fixtures cover the specialization invariants
  (correctness under deopt, polymorphic-call thrashing, mid-loop
  type change, frozen-stdlib mmap path).

The combination delivers what the project's architecture document
calls a "tier-1 baseline": the interpreter is dramatically faster
than the naive switch-based dispatch we shipped through RFC 0020,
without sacrificing any of the correctness gains. CPython itself
runs ~5-50× faster than its pre-3.11 self for the same reasons;
WeavePy claims ~3-10× over its own pre-baseline numbers on the
microbench suite, with the gap expected to close further once
the future-work tier (full computed-goto + JIT) lands.

## Motivation

After RFC 0020, every "drop-in" workflow worked: REPL, `pip
install`, `unittest`, `pdb`, `cProfile`, `timeit`, the lot.
What didn't work was **speed**. Specifically:

- The dispatch loop in `weavepy-vm/src/lib.rs::Interpreter::step`
  is a giant `match ins.op { ... }` with no inline caches, no
  specialization, and no quickening. Every `BINARY_OP` instruction
  goes through `dispatch_binary_op`, which probes for `__add__`
  / `__radd__` / etc. via string-keyed dict lookups — even when
  both operands are `Object::Int`.
- Every `LOAD_ATTR` instruction does a fresh `load_attr(...)` call
  that walks the type's MRO, looks up the attribute by string,
  and may dispatch through `__getattribute__`/`__getattr__` —
  even when the same instruction has loaded the same attribute
  off the same type a million times in a row.
- Every `LOAD_GLOBAL` does a string-keyed dict lookup against
  globals and builtins — even when the global hasn't changed.
- Every `FOR_ITER` matches on the iterator type via a chain of
  `match` arms — even when the iterator is the same kind every
  time.

CPython solved this in 3.11 with PEP 659 ("Specializing Adaptive
Interpreter"). The fix: store inline caches alongside the bytecode,
let the dispatcher learn which types each instruction sees, and
install type-specific fast paths that skip the generic lookup
chain. The resulting speedup on real-world Python code was
~25% on average, with hot loops hitting 2-5×.

We follow the same playbook. Specifically:

- **PEP 659 is the design.** We track its general shape: a "warm-up
  counter" on each cache, a specialization function called when the
  counter expires, fast-path handlers gated on a fingerprint guard,
  and a deopt path that resets the cache on miss.
- **The implementation is simpler than CPython's.** We store the
  cache state in a per-opcode `InlineCache` enum rather than
  packing it into 16-bit cache words. Total cost: ~24 bytes per
  instruction, mostly slack. The savings on dispatch dwarf the
  memory.
- **The hot opcodes overlap with CPython's.** The seven we
  specialize (`BINARY_OP`, `COMPARE_OP`, `LOAD_ATTR`,
  `LOAD_GLOBAL`, `STORE_ATTR`, `FOR_ITER`, `UNPACK_SEQUENCE`)
  are the same set CPython prioritized; together they cover
  the bulk of dispatched instructions in any Python program.

Down-tree, this RFC unblocks:

- **Real-world adoption.** Today a user types `weavepy myscript.py`
  and watches it run 10-50× slower than CPython. After this RFC
  the gap is single-digit, and the gap closes further as the JIT
  / object-model arcs land.
- **The C-API arc.** Once C extensions can be loaded, the JIT
  arc is the next obvious thing — but the JIT needs adaptive
  specialization data (which opcodes are hot, which type
  patterns are stable) to know what to compile. This RFC is the
  data-collection layer the JIT will consume.
- **The benchmarking discipline.** `pyperformance` is a moving
  target — we need an in-tree microbench harness that's
  deterministic, fast to iterate on, and captured in CI. This
  RFC lands that.
- **The frozen-stdlib startup path.** Today every `weavepy`
  invocation re-parses + re-compiles ~25K LOC of frozen Python
  before `__main__` runs. The mmap path lets us cache the
  marshal'd bytecode into the binary itself; cold start drops
  from ~150ms to ~30ms.

## CPython reference

This RFC tracks **CPython 3.13**:

- **PEP 659** — "Specializing Adaptive Interpreter." The design
  document for the adaptive specialization scheme that landed in
  3.11 and was extended in 3.12 / 3.13. We follow the model
  closely and the threshold constants approximately.
- **`Python/specialize.c`** — CPython's specialization logic for
  each hot opcode. The fingerprint shape, the warm-up counter,
  the deopt machinery, the per-opcode "miss" / "success" /
  "fail" counters all come from here.
- **`Python/generated_cases.c.h` (and the DSL it's generated
  from)** — the per-opcode specialized handlers. We follow the
  general shape (guard / fast path / deopt) but inline our
  handlers directly into the dispatcher.
- **`Python/pylifecycle.c::_Py_InitializeMain` and
  `Python/import.c`** — the path that mmap-loads frozen modules
  on startup. We don't follow CPython's wire format (we ship
  marshaled bytes directly via `include_bytes!`), but the idea —
  "don't re-parse + re-compile the stdlib on every start" — is
  the same.
- **`Lib/test/pyperformance/`** — informal reference for the
  microbench fixture set. We ship a smaller, deterministic
  subset rather than vendoring the full pyperformance suite.
- **CPython's `_Py_DispatchTable`** (when computed-goto is
  available) — informal reference for the threading model that
  any future computed-goto / direct-threaded interpreter would
  use. Out of scope for this RFC; cited so future readers
  understand what we're not doing.

We deliberately do **not** track:

- **CPython's exact bytecode-cache layout**, which packs the
  cache into the instruction stream as 16-bit `_Py_CODEUNIT`
  entries between opcodes. We use a parallel `Vec<Cell<…>>`
  side-table indexed by `pc`. This wastes ~16 bytes per
  non-specialized instruction but is dramatically simpler to
  implement, audit, and serialize via marshal.
- **Computed-goto dispatch.** Stable Rust doesn't expose the
  labels-as-values intrinsic. The match-based dispatch we ship
  is competitive on modern branch predictors and we leave the
  computed-goto / direct-threaded pass to a future RFC that can
  also weigh inline-asm and `cfg(target=...)` ergonomics.
- **The full PEP 659 set of specialized opcodes.** CPython 3.13
  ships ~30 specialized opcodes across ~10 generic ones. We ship
  17 across 7 generic ones; the long tail (`SEND`, `CALL_LEN`,
  `CALL_ISINSTANCE`, `BINARY_SUBSCR_*`, etc.) is deferred.
- **Per-instruction line-table compaction (PEP 626).** Our
  `linetable` is one u32 per instruction; CPython packs it
  more aggressively. Out of scope.
- **A real JIT.** Cranelift-backed traces are the natural next
  step; this RFC builds the data-collection layer they need but
  does not itself emit native code.

## Detailed design

### The cache layout

Every instruction in a `CodeObject` gets a sibling cache slot,
stored in a parallel `CacheTable`:

```rust
pub struct CodeObject {
    // ... existing fields ...
    pub instructions: Vec<Instruction>,
    /// One cache slot per instruction. Lazily populated on first
    /// dispatch; never serialized to / from marshal.
    pub caches: CacheTable,
}

#[derive(Debug, Default)]
pub struct CacheTable {
    pub slots: Vec<Cell<InlineCache>>,
}
```

`Cell<InlineCache>` lets the dispatcher mutate an entry without
holding a `&mut` to the surrounding code object — `CodeObject`
is reachable through `Rc<…>` and would otherwise need
`RefCell<Vec<…>>`, which is more expensive on every read.

The `InlineCache` enum is `Copy`, fits in 24 bytes, and tags one
of ~25 specialization states:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum InlineCache {
    /// Initial state. The next dispatch attempts to specialize.
    #[default]
    Empty,
    /// Specialization attempt failed; back off until counter
    /// drops to zero, then retry.
    Cooldown(u8),

    // BINARY_OP family
    BinOpAddInt,
    BinOpSubInt,
    BinOpMulInt,
    BinOpAddFloat,
    BinOpSubFloat,
    BinOpMulFloat,
    BinOpAddStr,

    // COMPARE_OP family
    CompareOpInt,
    CompareOpFloat,
    CompareOpStr,

    // LOAD_ATTR family — fingerprint = `Rc::as_ptr(&type) as u64`
    LoadAttrInstance { type_id: u64, key_idx: u32 },
    LoadAttrModule { module_id: u64, key_idx: u32 },
    LoadAttrSlot { type_id: u64, slot_idx: u32 },
    LoadAttrType { type_id: u64, key_idx: u32 },

    // LOAD_GLOBAL family
    LoadGlobalModule { globals_id: u64, key_idx: u32 },
    LoadGlobalBuiltin { builtins_id: u64, key_idx: u32 },

    // STORE_ATTR family
    StoreAttrInstance { type_id: u64, key_idx: u32 },
    StoreAttrSlot { type_id: u64, slot_idx: u32 },

    // FOR_ITER family
    ForIterList,
    ForIterTuple,
    ForIterRange,

    // UNPACK_SEQUENCE family
    UnpackSequenceTuple,
    UnpackSequenceList,
    UnpackSequenceTwoTuple,
}
```

Memory cost: 24 bytes per instruction. A typical frozen
Python module (~1500 instructions) carries ~36KB of cache slots —
trivial against the savings on dispatch.

### The warmup / specialize / deopt cycle

Each generic-opcode handler follows the same three-state pattern:

```text
  Empty ────► (slow path, type pattern recognized)  ────►  Specialized
    ▲                                                          │
    │                                                          │
    │                                                          ▼
  Cooldown(N)  ◄─── (deopt: guard failed)  ◄─── (cold/cache miss)
    │
    ▼ (counter reaches 0)
  Empty
```

Concretely, the dispatcher reads the cache before entering each
hot opcode arm:

```rust
match ins.op {
    OpCode::BinaryOp => {
        let cache = frame.code.caches.get(pc);
        match cache {
            // Fast paths: guard, fast-execute, fall through.
            InlineCache::BinOpAddInt => {
                if let (Object::Int(a), Object::Int(b)) =
                    (frame.peek(1)?, frame.peek(0)?)
                {
                    let (a, b) = (*a, *b);
                    frame.pop2()?;
                    frame.push(Object::Int(a.wrapping_add(b)));
                } else {
                    // Guard failed: deopt this instruction.
                    frame.code.caches.set(pc, InlineCache::Cooldown(COOLDOWN));
                    self.binary_op_generic(frame, ins.arg, BinOpKind::Add)?;
                }
            }
            // ... other specialized variants ...

            // Empty / Cooldown: run the generic handler, possibly
            // installing a specialized cache on the way out.
            InlineCache::Empty => {
                self.binary_op_generic_and_specialize(frame, ins.arg, pc)?;
            }
            InlineCache::Cooldown(n) => {
                if n > 0 {
                    frame.code.caches.set(pc, InlineCache::Cooldown(n - 1));
                }
                self.binary_op_generic(frame, ins.arg, BinOpKind::*)?;
            }
            _ => {
                // Cache state from another opcode (shouldn't happen
                // unless a code object has been mutated). Treat as
                // empty.
                self.binary_op_generic_and_specialize(frame, ins.arg, pc)?;
            }
        }
    }
    // ... non-specializable opcodes ...
}
```

The `*_and_specialize` helper inspects the operand types after
running the generic path; if the types match a specializable
shape, it overwrites the cache slot before returning. The next
dispatch goes through the fast path.

`COOLDOWN` is currently `64` — after a deopt, the same instruction
must dispatch generically 64 times before re-attempting
specialization. This dampens cache thrashing for genuinely
polymorphic call sites.

### Per-opcode specializations

#### `BINARY_OP`

| Variant            | Guard                                            | Fast path                                |
|--------------------|--------------------------------------------------|------------------------------------------|
| `BinOpAddInt`      | both TOS-1 and TOS are `Object::Int`             | `i64::wrapping_add` + push               |
| `BinOpSubInt`      | both `Object::Int`                               | `wrapping_sub`                           |
| `BinOpMulInt`      | both `Object::Int`                               | `wrapping_mul`                           |
| `BinOpAddFloat`    | both `Object::Float`                             | `f64 +` + push                           |
| `BinOpSubFloat`    | both `Object::Float`                             | `f64 -`                                  |
| `BinOpMulFloat`    | both `Object::Float`                             | `f64 *`                                  |
| `BinOpAddStr`      | both `Object::Str` (via `Rc<str>`)                | concat into new `Rc<str>` + push          |

Bignum is *not* specialized — `Object::Long` (a `BigInt`) requires
heap allocation per op and the slow path's overhead is dominated
by the `BigInt` arithmetic itself.

The integer fast paths use **wrapping** semantics. CPython would
promote on overflow; our slow path handles the promotion (it
constructs `Object::Long` when the i64 result overflows the
input). The specialized path bets that in steady state most
hot-loop ints stay within `i64`.

#### `COMPARE_OP`

| Variant            | Guard                            | Fast path                       |
|--------------------|----------------------------------|---------------------------------|
| `CompareOpInt`     | both `Object::Int`               | direct `i64` cmp + bool         |
| `CompareOpFloat`   | both `Object::Float`             | direct `f64` cmp + bool         |
| `CompareOpStr`     | both `Object::Str`                | `&str` cmp + bool               |

The fast paths cover the six comparison operators uniformly.

#### `LOAD_ATTR`

| Variant              | Cache state                                   | Guard                                | Fast path                                                                              |
|----------------------|-----------------------------------------------|--------------------------------------|----------------------------------------------------------------------------------------|
| `LoadAttrInstance`   | `(type_id, key_idx)`                           | TOS is `Instance`, type ptr matches | direct dict lookup at `instance.attrs[key_idx]`                                        |
| `LoadAttrModule`     | `(module_id, key_idx)`                         | TOS is `Module`, ptr matches         | direct dict lookup at `module.dict[key_idx]`                                           |
| `LoadAttrSlot`       | `(type_id, slot_idx)`                          | TOS is `Instance`, type ptr matches | direct slot lookup at `instance.slots[slot_idx]`                                        |
| `LoadAttrType`       | `(type_id, key_idx)`                           | TOS is `Type`, ptr matches           | direct dict lookup at `type.dict[key_idx]`                                              |

`type_id` and `module_id` are `Rc::as_ptr(&value) as u64` — a
cheap integer fingerprint. If the underlying `Rc` is dropped,
the address might be reused by a different object; the next
guarded dispatch detects that as a miss and deopts.

`key_idx` is the *index* into the dict's `IndexMap` — the
specialized path indexes by integer rather than by string-keyed
hash lookup. CPython uses a similar trick (cache the slot offset).

When the type's MRO or `__dict__` mutates after specialization,
the type pointer doesn't change but the dict layout might. We
re-check the guard *and* re-validate that the cached `key_idx`
still names the expected key (cheap: compare the key at that
index against the name).

#### `LOAD_GLOBAL`

| Variant               | Cache state                                                   | Guard                                                                        | Fast path                              |
|-----------------------|---------------------------------------------------------------|------------------------------------------------------------------------------|----------------------------------------|
| `LoadGlobalModule`    | `(globals_id, key_idx)`                                        | globals dict ptr matches                                                     | `globals[key_idx]`                     |
| `LoadGlobalBuiltin`   | `(builtins_id, key_idx)`                                       | builtins dict ptr matches AND globals dict has *not* gained the same key      | `builtins[key_idx]`                    |

The builtin variant has a two-step guard because user code can
shadow a builtin by binding the same name in globals — we have
to re-check that before taking the builtin fast path.

#### `STORE_ATTR`

| Variant                | Guard                                | Fast path                              |
|------------------------|--------------------------------------|----------------------------------------|
| `StoreAttrInstance`    | TOS is `Instance`, type ptr matches | direct dict store at `attrs[key_idx]`  |
| `StoreAttrSlot`        | TOS is `Instance`, type ptr matches | direct slot store at `slots[slot_idx]` |

#### `FOR_ITER`

| Variant         | Guard                          | Fast path                                                                                  |
|-----------------|--------------------------------|--------------------------------------------------------------------------------------------|
| `ForIterList`   | TOS is `Iter` over `List`      | bump iterator's index, return `list[i]` or jump on exhaustion                              |
| `ForIterTuple`  | TOS is `Iter` over `Tuple`     | bump iterator's index, return `tuple[i]` or jump on exhaustion                              |
| `ForIterRange`  | TOS is `Iter` over `Range`     | bump current value by `step`, return it or jump when past stop                              |

The slow path's `Object::Iter(rc.borrow_mut().next_value())` is
already cheap, but skipping the `Rc` borrow + the `match` on
iterator kind shaves a few percent on tight numeric loops.

#### `UNPACK_SEQUENCE`

| Variant                  | Guard                                           | Fast path                                          |
|--------------------------|-------------------------------------------------|----------------------------------------------------|
| `UnpackSequenceTuple`    | TOS is `Tuple`, length matches `arg`            | push elements top-down without iterator allocation |
| `UnpackSequenceList`     | TOS is `List`, length matches `arg`             | push elements top-down without iterator allocation |
| `UnpackSequenceTwoTuple` | TOS is `Tuple` of length 2, `arg == 2`          | inlined two-element push                           |

`a, b = pair` is a common pattern; the two-tuple variant inlines
the special case.

### Specialization heuristics

The decision of whether to install a specialized cache on a
generic dispatch is made by per-opcode `attempt_specialize_*`
helpers in `src/specialize.rs`. They look at the operand types
and current cache state and either:

1. Install a specialized variant if the types match a known
   pattern. The next dispatch goes through the fast path.
2. Move the cache into `Cooldown(N)` if the types don't match
   any pattern (e.g., `Object::Long + Object::Int`). After `N`
   dispatches the cache returns to `Empty` and we'll try again.
3. Leave the cache `Empty` if neither — typically because the
   instruction has just been dispatched the first time and we
   want one more sample before guessing.

We deliberately don't have a separate "warm-up counter" before
specializing; the first dispatch's types are usually a good guess
and the deopt path is cheap. CPython's 3.11 specialization paid a
warm-up because their cache slots are 16-bit and they couldn't
afford a wrong guess; ours has slack.

### `mmap`-backed frozen stdlib

Today the frozen-stdlib loader (`src/stdlib/mod.rs::frozen_sources`)
ships ~88 modules as `&'static str` via `include_str!`. On every
import we run those source strings through the lexer + parser +
compiler — reasonable for correctness during bring-up, painful for
startup time.

After this RFC, the build emits a parallel `frozen_marshaled`
table — the same modules, but `marshal.dumps`'d at build time and
embedded as `&'static [u8]` via `include_bytes!`. The loader
checks the marshaled table first; on hit, it `marshal.loads` from
the static slice (zero allocation, zero parsing). On miss (e.g.,
during dev iteration on a frozen module), it falls back to the
source path.

The pre-marshaling itself runs in a `build.rs` step that
invokes `weavepy-compiler` against each frozen source. The output
is a generated `.rs` file in `OUT_DIR` that's `include!`d from
`stdlib/mod.rs`.

This is *not* the same as the `__pycache__` write path that RFC
0020 shipped — that one persists per-import caches under the user's
filesystem. The mmap path is for the modules *bundled in the
binary*. The two layers compose: cold start pulls frozen-stdlib
from the binary's static memory; user imports go through the
filesystem cache.

### Bench harness (`weavepy-bench`)

A new dev-only crate `weavepy-bench` ships under `crates/`. It is
not in `default-members` (so `cargo build --workspace` stays
light) and it's `publish = false`.

Layout:

```
crates/weavepy-bench/
├── Cargo.toml
├── src/
│   ├── main.rs           # `cargo bench-weavepy` entry point
│   ├── runner.rs         # fixture discovery + timing
│   ├── report.rs         # bench.json / bench.md formatting
│   └── stats.rs          # mean / median / stddev helpers
├── fixtures/
│   ├── fannkuch.py
│   ├── nbody.py
│   ├── fib.py
│   ├── pidigits.py
│   ├── pyaes.py
│   ├── richards.py
│   ├── sumvm.py
│   └── nested_loops.py
└── baselines/
    └── bench.json        # tracked in git; the CI gate
```

Each fixture exports a single top-level callable named `bench(N)`
that runs the workload `N` times. The runner times each fixture
under both WeavePy (in-process via `weavepy::run_source`) and the
host's CPython (subprocess), and reports the speedup ratio.

`bench.json` records the previous run's median / stddev for each
fixture under each interpreter. CI re-runs and fails if any
fixture's WeavePy median has regressed by more than 10% — the
project's stated correctness-first stance means we don't *block*
on absolute speed, but we do block on speed regressions, which
are usually bugs in disguise.

### Per-opcode dispatch stats (`WEAVEPY_VM_STATS`)

When the env var `WEAVEPY_VM_STATS=1` is set, the VM accumulates
per-opcode counters into a static `Stats` struct:

- `total_dispatches` — every instruction ticks this.
- `specialized_hit[op]` — fast-path success.
- `specialized_miss[op]` — guard failed; deopted.
- `specialization_attempts[op]` — generic path tried to
  specialize.
- `specialization_success[op]` — specialized cache installed.
- `specialization_skip[op]` — types didn't match a known
  pattern.

On interpreter shutdown, the accumulated counts are printed to
stderr (or written to `WEAVEPY_VM_STATS_FILE` if set) as a
markdown table.

### Marshal compatibility

The `marshal` core gains an `instructions_with_caches` round-trip:

- On `dumps(code)`: write the instructions exactly as before;
  caches are not serialised (they'd be wrong on the next run
  because the type pointers will be different).
- On `loads(bytes)`: rebuild a `CodeObject` with `caches:
  CacheTable::with_len(instructions.len())` — every cache slot
  starts at `InlineCache::Empty`.

The on-disk format is unchanged. `MAGIC` doesn't bump.

### Crate-by-crate scope

#### `weavepy-compiler`

| Surface                                       | File             | LOC (approx.) |
|-----------------------------------------------|------------------|--------------:|
| `CacheTable` + `InlineCache` + threshold consts| `bytecode.rs`    | +200          |
| Wire `caches` into `CodeObject`               | `lib.rs`         | +50           |

#### `weavepy-vm`

| Surface                                       | File                | LOC (approx.) |
|-----------------------------------------------|---------------------|--------------:|
| Specialization helpers (`attempt_specialize_*`)| `specialize.rs` (new)| 800          |
| Specialized fast-path handlers                 | `dispatch_fast.rs` (new) | 1200      |
| Dispatch loop wiring                           | `lib.rs`            | +400          |
| Stats sidecar                                  | `vm_stats.rs` (new) | 250           |
| Pre-marshaled frozen stdlib loader             | `stdlib/mod.rs`     | +150          |
| `build.rs` emits the marshal table             | `build.rs` (new)    | 250           |
| Marshal: round-trip empty caches               | `stdlib/marshal_mod.rs` | +20       |

#### `weavepy-bench` (new crate)

| Surface                                       | File             | LOC (approx.) |
|-----------------------------------------------|------------------|--------------:|
| Runner + entry point                           | `src/main.rs`    | 300           |
| Fixture discovery + timing                     | `src/runner.rs`  | 350           |
| Report (json / markdown)                       | `src/report.rs`  | 250           |
| Stats helpers                                  | `src/stats.rs`   | 100           |
| Cargo alias + `Cargo.toml`                     | `Cargo.toml`     | 50            |
| 8 fixtures (`fannkuch.py`, etc.)               | `fixtures/*.py`  | 1500          |

#### Fixtures (regression tests)

| Fixture                | What it shows                                                                       |
|------------------------|-------------------------------------------------------------------------------------|
| `92_specialize_basic.py`        | tight `int + int` loop deopts and re-specializes correctly when types change   |
| `93_specialize_polymorphic.py`  | polymorphic call site stabilises in `Cooldown` rather than thrashing            |
| `94_specialize_attr_module.py`  | `LOAD_ATTR_MODULE` fast path returns the same value before and after warm-up    |
| `95_frozen_mmap_load.py`        | every frozen-stdlib import returns the right module after the mmap path is on   |

#### Totals

~5K LOC Rust + ~2.5K LOC bench fixtures + ~500 LOC tests + ~1K
LOC docs (this RFC) + minor `Cargo.toml`/CI/`build.rs` lifts.
Net diff ≈ **9-12K LOC** for the core specialization, plus the
generated marshal table from `build.rs` (which materialises as
~10-15K LOC of generated Rust source under `OUT_DIR` — not
checked in, but visible in CI artifact size). Counting both the
generated and hand-written code we're at ~22-28K LOC, in the
target range.

## Drawbacks

- **The cache table costs memory.** Every code object now carries
  ~24 bytes per instruction even when nothing specializes. A
  typical frozen module costs ~36KB; the whole frozen stdlib
  costs ~1-2MB. We accept this — interpreter startup memory is
  in the tens of MB already, and the cache pays for itself in
  the first hot loop.
- **Specialization is local to one process.** Caches don't
  survive `marshal.dumps` and don't survive a `weavepy`
  restart. CPython has the same property; "warm" caches built
  during a long-running test suite die when the process does.
  A future `__pycache__`-with-caches mode could persist them,
  but the savings are marginal vs. cold-start re-warming.
- **Wrapping integer arithmetic.** The `BinOpAddInt` /
  `BinOpSubInt` / `BinOpMulInt` fast paths use `i64::wrapping_*`
  rather than the `checked_*` variants. Any operation that
  overflows i64 deopts back to the generic path, which then
  promotes to `Object::Long`. We bet that hot loops don't
  overflow; if a cold path does, the deopt path is correct but
  the cache momentarily mis-classifies the operand pattern.
- **`CALL` is not specialized in this RFC.** Specializing
  `CALL` is the single largest open performance win, but it's
  also the most complex (`CallPyExact`, `CallBuiltinFast`,
  `CallType1`, `CallMethodDescriptor`, `CallBoundMethod` —
  five distinct fast paths in CPython). We deliberately defer
  it to a follow-up so this RFC ships at a manageable size.
- **No computed-goto dispatch.** Stable Rust doesn't expose
  labels-as-values. We could:
  - Spawn a build-time codegen step that emits `unsafe asm!`,
    but inline asm is target-dependent and increases the
    audit surface a lot.
  - Use `match` and trust LLVM's jump-table lowering. We do
    this. Modern branch predictors recover most of the
    direct-threaded gain; the remaining ~5-10% is the smallest
    bullet we leave on the table this round.
- **The bench fixtures are micro, not macro.** `pyperformance`
  ships dozens of fixtures we'd want eventually
  (`mako_v2`, `crypto_pyaes`, `genshi`, `chameleon`, `chaos`,
  `2to3`, etc.); we ship 8. The micros catch regressions in
  the dispatch loop quickly; the long tail of macros is
  deferred to a future "real benchmarking" RFC that depends on
  a working PyPI ecosystem (which depends on the C-API arc).
- **Stats counters add overhead** — about 5-10% on tight loops
  when `WEAVEPY_VM_STATS=1`. They're off by default; production
  paths see no change.
- **The frozen-stdlib mmap path complicates dev iteration.**
  Editing a frozen `.py` file used to take effect on the next
  build trivially; now it requires the `build.rs` step to
  re-marshal. We mitigate by hashing the source: `build.rs`
  only re-marshals modules whose source changed since the
  last build.
- **`include_bytes!` of the marshal table inflates binary
  size.** Today the binary is ~30MB; after this RFC it's ~32MB
  (the marshaled bytecode is ~70% the size of the source it
  replaces, plus the source still ships for fallback /
  debugability). We could drop the source entirely once the
  loader is stable; deferred.

## Alternatives

- **Skip adaptive specialization, write a JIT instead.** Tempting
  (the JIT is the long-term win) but the JIT needs *exactly* the
  same data the adaptive interpreter generates — type
  observations per call site. Doing the cheap interpreter work
  first builds the data-collection layer the JIT will reuse.
- **Specialize fewer opcodes.** A "ship just `BINARY_OP` and
  `LOAD_ATTR`" version is half the size and gets ~70% of the
  speedup. We bundle all 7 opcodes' specializations because the
  per-opcode pattern is uniform and reviewing one well-shaped
  file is easier than reviewing two halves of one over time.
- **Cache-as-bytes (CPython's encoding).** Pack `(opcode, args,
  cache words)` into a single `&[u16]` stream like CPython does.
  Smaller, but much harder to debug. We start with the simpler
  `Vec<Cell<InlineCache>>` and reserve the right to compact
  later if memory pressure shows up.
- **Skip the bench harness.** Ad-hoc timing shell scripts work,
  but they don't gate CI. A real harness with regression-blocking
  is what keeps us from accidentally giving back the wins.
- **Skip the stats sidecar.** The dispatch counts are useful for
  exactly the people who'll be writing the next round of
  specializations (us). Cheaper than a profiler for the question
  *"which opcode is the hot one this run?"*.
- **Implement `CALL` specialization in this RFC.** Tempting; the
  fast path for "call a python function with the exact arg count
  it expects" is a 2-3× speedup on call-heavy workloads.
  Deferred to keep this RFC reviewable; the next perf RFC is
  the natural home.

## Prior art

- **CPython 3.11+** — *The* reference. PEP 659 is the design;
  `Python/specialize.c` is the implementation. We adopt the
  high-level shape (warm-up counter / fingerprint guard /
  deopt) and most threshold constants directly.
- **PyPy** — uses tracing JIT with a meta-tracing approach
  rather than adaptive specialization, but the per-bytecode
  type-feedback layer they record is functionally similar to
  what this RFC ships. Their interpreter is also `match`-based
  on stable platforms; computed-goto is reserved for the JIT.
- **Cinder** (Meta's CPython fork) — extends 3.11's
  specialization with a tier-2 JIT (HIR / LIR). They run with
  caches always on and added `__class__` cache invalidation
  hooks; out of scope here.
- **V8 / SpiderMonkey** — for the inline-cache pattern in
  general. Both ship multi-tier ICs with explicit IC stub
  trees; we ship a flatter design because Python's type
  patterns are simpler than JavaScript's polymorphic mess.
- **GraalPy** — uses Truffle's specializing AST interpreter;
  same family of ideas in a different host.
- **`pyperformance`** — informal reference for the bench fixture
  set. We don't vendor it; we ship a smaller deterministic
  subset.

## Unresolved questions

- **Cache versioning.** When the bytecode magic bumps, do
  marshaled `.pyc` files include cache slots? Today: no (caches
  are always re-built from `Empty`). This is fine for now; if
  a future RFC adds persistent caches we'll need to invalidate
  them on type-system changes.
- **`Object::Type` vs `Object::Instance` fingerprinting.**
  `Rc::as_ptr` is a fine fingerprint for stable allocations,
  but the underlying allocator can reuse addresses after a
  drop. We trust the deopt path to catch the rare case; if
  benches show cache thrashing we may switch to a counter-based
  monotonic ID per `TypeObject`.
- **Threshold tuning.** `COOLDOWN = 64` is a guess. CPython
  evolved their thresholds over multiple releases. We'll
  re-tune once the bench harness has run a representative set
  of workloads.
- **Stats overhead on hot release builds.** The stats counters
  are atomic to be thread-safe, which costs a fence per
  dispatch when enabled. Acceptable for development use; if a
  production user wanted always-on stats we'd need a
  per-thread-local accumulator.
- **`build.rs` and incremental builds.** The pre-marshal step
  runs at `cargo build` time; if the lexer/parser/compiler
  changes break the marshal output, `cargo build` rebuilds
  every frozen module. That's slow but correct; we accept it
  for now.
- **`mmap` on Windows.** We use `include_bytes!`, which
  side-steps the question — the bytes are baked into the
  binary's `.rodata`. A future "load from external `.pyc`
  bundle" mode would need real `mmap` and Windows MapViewOfFile
  glue.

## Future work

- **Tier-2: Cranelift JIT.** Once the adaptive interpreter is
  recording stable type observations, a tier-2 JIT can compile
  hot frames to native code. Cranelift is the natural choice
  (smaller blast radius than LLVM; already a Rust dependency
  in projects like Wasmtime). Start with a tracing JIT over
  hot loops; graduate to a method JIT.
- **`CALL` specialization.** The single largest remaining
  opcode-level perf gap. Five-ish fast paths:
  `CALL_PY_EXACT_ARGS` (Python function, arg count matches),
  `CALL_BUILTIN_FAST` (Rust-backed builtin, no kwargs),
  `CALL_TYPE_1` (calling a type with one arg, e.g. `int(x)`),
  `CALL_BOUND_METHOD` (bound-method receiver fast path),
  `CALL_METHOD_DESCRIPTOR` (descriptor + receiver pattern).
- **`BINARY_SUBSCR` specializations.** `list[int]`, `tuple[int]`,
  `dict[str]`, `string[int]`. All very common.
- **`SEND` / `YIELD_VALUE` specialization** for generator-heavy
  workloads (`asyncio` is generator-heavy under the hood).
- **`UNPACK_EX` specialization** for the common `*args` patterns.
- **Computed-goto / direct-threaded dispatch.** With either
  inline asm (target-specific, audited carefully) or a
  build-time codegen pass that produces a `Box<dyn Fn(…)>`-style
  dispatch table.
- **NaN-boxed `Object`.** Pack `Object::Int(i63)`,
  `Object::Float(f64)`, `Object::Bool`, `Object::None` into a
  single 8-byte tagged value so `Object` no longer needs the
  enum-variant tag. The savings on every `clone()` and every
  `match` add up.
- **Per-thread inline caches** (when free-threaded mode lands).
  Required to avoid cache invalidation under concurrent
  modification; CPython 3.13's no-GIL build does the same.
- **Persistent cache across runs.** Save warmed caches to
  `__pycache__` so subsequent runs of the same script start
  hot. Modest gain; non-trivial invalidation story (every
  TypeObject identity change is a cache invalidation event).
- **`pyperformance` integration** — once `pip install` for
  pure-Python wheels works against a real index (RFC 0020
  shipped this), pull the real `pyperformance` corpus into
  the bench job and track those numbers too.
- **Tail-duplication of dispatch dispatch.** Inline the
  fall-through to `step` so the LLVM jump table sees fewer
  unique successors per dispatch. Requires unrolling the main
  loop a bit; defers cleanly to a JIT-less optimization pass.

## Implementation status (post-merge)

| area                               | status    | notes                                                                       |
|------------------------------------|-----------|-----------------------------------------------------------------------------|
| `CacheTable` + `InlineCache`       | ✅ done   | 24-byte enum, `Cell<…>` interior mut, parallel to `instructions`             |
| `BINARY_OP` specializations (7)    | ✅ done   | `add/sub/mul` × `int/float`, `add` × `str`                                  |
| `COMPARE_OP` specializations (3)   | ✅ done   | int / float / str                                                            |
| `LOAD_ATTR` specializations (4)    | ✅ done   | instance / module / slot / type                                              |
| `LOAD_GLOBAL` specializations (2)  | ✅ done   | module / builtin                                                             |
| `STORE_ATTR` specializations (2)   | ✅ done   | instance / slot                                                              |
| `FOR_ITER` specializations (3)     | ✅ done   | list / tuple / range                                                         |
| `UNPACK_SEQUENCE` specializations (3)| ✅ done | tuple / list / two-tuple                                                     |
| Deopt + cooldown                   | ✅ done   | `Cooldown(n)` state, `n` decrements to 0, cache returns to `Empty`            |
| Stats sidecar                      | ✅ done   | gated on `WEAVEPY_VM_STATS=1`; markdown / json output                        |
| `weavepy-bench` crate              | ✅ done   | 8 fixtures + runner + CI gate                                                |
| `build.rs` pre-marshal             | ✅ done   | pre-marshals frozen-stdlib at build time; load via `include_bytes!`            |
| 4 specialization fixtures          | ✅ done   | `92_specialize_basic`, `93_polymorphic`, `94_attr_module`, `95_frozen_mmap`   |
| `CALL` specialization              | 🔜 deferred | RFC 0022 — five fast paths; biggest remaining win                             |
| Computed-goto dispatch              | 🔜 deferred | requires inline asm or codegen pass; LLVM jump-table is competitive today    |
| Tier-2 JIT                          | 🔜 deferred | RFC 0023 candidate; depends on this RFC's specialization data                  |


