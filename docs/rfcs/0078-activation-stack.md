# RFC 0078: One activation stack for the interpreter, the frame JIT, and generators

- **Status**: Draft
- **Authors**: WeavePy authors
- **Created**: 2026-10-20
- **Tracking issue**: TBD
- **Builds on**: RFC 0065 (the quiet loop), RFC 0067 (direct calls between
  compiled functions), RFC 0069 and RFC 0070 (inline generator resumes),
  and the measurements in `docs/PERFORMANCE-PROGRESS.md`.

## Summary

A Python-to-Python call costs WeavePy about four times what it costs
CPython 3.14, and a generator's creation, resume and teardown cost about
three times. The cost isn't in any one step. It's the activation protocol:
an activation is a pooled, boxed `InlineAct` holding a `Frame` whose
locals and operand stack are separate vectors, made visible to
introspection through a lazy pending list, switched to through the core
loop's `CoreSwitch`, and entered by native code through a 17-field
`State` built per call. Each of those layers is cheap; together they're
about 400 instructions a call.

This RFC proposes replacing them with one per-thread activation stack, as
CPython 3.11 did: each activation a record in a contiguous, growable
stack, holding its locals and operand stack inline and a link to its
caller. The interpreter, the frame JIT and generators all run activations
as records on that stack (a generator's record lives in the generator
while it's suspended). The lazy pending list, the activation pool, the
per-site warm slots, `LeanFrame`, the frameless leaf evaluator and the
native `State` all go away.

## Motivation

Measured on `4a2ca7f`, a `quick` build, by callgrind and instructions
retired:

| Operation | WeavePy | CPython 3.14 |
| --- | ---: | ---: |
| Direct call and return between compiled functions | about 480 | about 100 |
| Core-loop inline call and return | about 400 | about 100 |
| Generator creation and teardown | about 1,200 | about 300 |
| Resume of a generator body the JIT doesn't compile | about 455 | about 100 |

Line-level attribution of the direct call shows no step above 20
instructions: about 60 checks, 20 for the site cache, 20 for the pool and
warm slot, 30 to bind arguments, 15 for frame handles, 20 for the
activation's fields, 20 for the pending list, 15 for the switch, 25 to
build the callee's `State`, 20 to enter native code, and about 100 to
check, release and restore on return. Calls are the largest bucket of the
suite's excess instructions (0.17 of CPython's whole count, against 0.07
for the interpreter's dispatch), and the parallel cheaper paths built to
avoid them (frameless pure and site leaves, constructor lanes, builtin
lanes, generator fast steps) are a large part of why WeavePy's hot code
footprint is about three times CPython's.

Two attempts on the current structure show its limits:

- **Light direct calls** bound a per-depth record without the switch or
  the pending list (`go` retired 6% fewer instructions). Anything that
  switches activations had to turn the record into an `InlineAct` first,
  which kept constructors, generator resumes and builtin lanes off it.
- **One bind for every direct call** (PR #86) unified the paths into the
  existing `InlineAct` at no gain: the protocol's cost is in what every
  activation must do, not in which path binds it.

## Detailed design

### The record

```rust
#[repr(C)]
struct Activation {
    prev: *mut Activation,      // the caller (the spine)
    code: *const CodeObject,    // borrowed: `func` holds it, or the record owns a count
    func: Object,               // the callable, owned
    globals: *const DictCell,
    builtins: *const DictCell,
    pc: u32,
    sp: u32,                    // operand stack depth
    flags: u32,                 // kind (plain, generator, class body, ...), direct, init
    frame_obj: Option<Rc<PyFrame>>,  // materialized frame object, rarely set
    rare: Option<Box<ActRare>>,      // exception state, cells, binop, chain, ...
    localsplus: [Object; 0],    // locals, then cells, then the operand stack
}
```

Records live in a per-thread stack of chunks (CPython's data stack): a
call bumps the top by the callee's `nlocals + ncells + stacksize`, writes
the header, and moves its arguments in place. A return releases the
locals and drops the top. A generator's record is copied into the
generator object when it first suspends, and linked onto the stack while
it runs.

### The spine

Introspection (`sys._getframe`, tracebacks, `f_back`, `inspect.stack`,
warnings' stack levels, `sys.settrace`) walks `prev` links from the
thread's top record. A frame object is materialized on demand and points
at its record; when the record dies, a frame object that outlives it takes
a copy of the locals (CPython's `take_ownership`). This replaces the lazy
pending list, `QuietShell`, `LeanAct` and the frame-shell pool.

### Execution

The core loop runs the top record. A call pushes the callee's record and
continues in the loop; a return pops it and continues the caller. The
frame JIT's compiled functions take the record (and a per-thread context)
as their arguments and address locals and the operand stack at fixed
offsets from it, so `State` disappears; a direct call between compiled
functions pushes the record and calls the callee's entry. An exit from
native code leaves the record exactly as the core loop would hold it, as
today.

### What goes away

`InlineAct` and its pool, `LeanFrame`, `QuietShell`, the pending list,
`CoreSwitch`'s entry and root bookkeeping, the per-site warm slots, the
direct-call `State`, and once calls are cheap, the frameless leaf
evaluator (`leaf_plan`) and the generator fast step (`gen_fast`).

## Staging

Each stage keeps the gates passing and lands on its own:

1. **Frame storage behind an interface.** `Frame::locals` and
   `Frame::stack` become types with the API their users need (no
   behavior change), so their representation can move.
2. **Records on a per-thread stack** for the activations the quiet loop
   runs inline (today's `InlineAct`), linked by `prev`; the pending list
   walks the links instead of its own vector.
3. **The frame JIT on records**: compiled code addresses the record
   directly, direct calls push records, `State` goes.
4. **Generators on records**, and the general `step` loop's frames.
5. **Delete** the frameless leaf evaluator and the generator fast step,
   once calls and resumes are as cheap as what they replaced.

## Drawbacks

This touches the busiest code in the VM (`quiet_run`, the core loop's
call and return arms, `frame_jit`'s calling convention, generators, frame
objects) and the behavior the CPython gate checks most closely: frame
introspection, tracing, tracebacks, generator lifetimes and finalizer
timing. Stages 2 to 4 are each large, and the speedup arrives mostly at
stages 3 and 5.

## Alternatives

- **Keep tuning the current paths.** Each local change is worth 1% to 5%
  on a few workloads; the measurements in the progress note show the
  remaining cost spread thinly enough that this converges slowly.
- **Inline small callees in the frame JIT.** This removes calls where it
  applies, but needs the same materialization of activations at exits,
  and the hottest calls in the suite recurse (`go`'s `Square.find`
  recurses on 66% of its calls), so it doesn't replace a cheap protocol.

## Expected results

If a call and return cost about 150 instructions and a generator's
lifecycle about 500, the call and generator buckets fall from 0.23 of
CPython's instruction count to about 0.08, the suite's instruction ratio
from 1.32 to about 1.17, and the hot code footprint shrinks with the
paths deleted at stage 5.

## Unresolved questions

- Whether greenlets (which switch native stacks) keep their own record
  stacks or share the thread's.
- How free-threaded mode's per-thread stacks interact with
  `sys._current_frames()`.
