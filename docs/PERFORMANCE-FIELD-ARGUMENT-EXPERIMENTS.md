# Borrowed field-argument experiments

The accepted simple-call reader handles local, constant, and small-integer
arguments. It doesn't fuse calls such as
`Strength.stronger(self.strength, self.my_output.walk_strength)`. The first
candidate adds up to eight guarded instance-field reads to that reader, keeping
each argument rooted through its original local or constant. It uses existing
primary dictionary/slot caches and preserves class versions, actual names,
private storage, callee/code/default checks, and ordinary callback fallback.
It adds no owners, metadata, JIT admission, or threshold changes.

This candidate is not admitted. Its complete field callers improve substantially,
but descriptor fallback costs repeat. An early cache check also fails to resolve those costs; both runtime
variants are held. The accepted runtime
remains `6ea3e53`; the earlier held conditional-getter and local-release changes
are absent from this experiment.

## Coverage and provenance

The new behavioral fixture passes CPython and the accepted runtime in all three
execution modes before the optimization. Its path-coverage test fails at zero
completed field-argument calls before the change and passes afterward. Each of
six 4,000-call probes records 3,998 fused calls with the JIT disabled and 2,953
with it enabled. Two complete DeltaBlue iterations record 242/210 calls.
Counters run after successful evaluation, so speculative reads don't qualify.

Coverage includes global/static/bound calls, dictionaries/slots, aliases,
reordered and deleted fields, missing fields, evaluation order, descriptors,
writable caller locals, rich-comparison frames, code/default/binding mutation,
argument and chain limits, returned ownership, tracing, and thread publication.
Reusable probes measure complete callers and descriptor/hook fallbacks,
including timed transitions after warmup. Setup and result checks remain timed.
The comparison tool records their environment selectors.

All 389 VM tests, embedding, VM Clippy with its existing exclusions, no-default
compilation, scoped formatting, 14 benchmark-tool tests, and selected CPython
fixtures pass. The frozen release passes 291 runs across 97 fixtures in
JIT/interpreter/GIL-disabled modes and 768 semantic probes. All 38 checked
compilation decisions and JIT statistics match the accepted baseline.

The release build takes 7 minutes, 30 seconds; its controller closes before
freezing. The executable remains 51,877,200 bytes, with SHA-256
`7aa9fee84ae8fabd9df30a9005e8a3b4e2004cec7f8e66b445ceb381bda854b5`.
The complete patch, including new files, is
`66e0bfeff2d00f949cdd8db0f92838df38bf7b26effee962ee85659c5a6bd00b`
against documentation commit `1b0affe`.

## Initial measurements

Measurements use macOS x86-64 and optimized CPython 3.14.5, the existing paired
methodology, isolated batches, alternating process order, discarded warmup, and
verified frozen caches. The baseline is accepted `6ea3e53`. Ratios below are
candidate/baseline; lower is better. Standard work is 200,000 calls over seven
cycles; sustained work is one million calls over five cycles.

| Complete caller | Standard JIT | Standard interpreter | Sustained JIT | Sustained interpreter |
| --- | ---: | ---: | ---: | ---: |
| Global, dictionary fields | 0.714 | 0.705 | 0.705 | 0.728 |
| Static, dictionary fields | 0.696 | 0.678 | 0.686 | 0.692 |
| Bound method, dictionary fields | 0.741 | 0.732 | 0.739 | 0.726 |
| Global, slot fields | 0.671 | 0.667 | 0.671 | 0.655 |
| Static, slot fields | 0.672 | 0.660 | 0.652 | 0.654 |
| Bound method, slot fields | 0.727 | 0.708 | 0.697 | 0.678 |
| Descriptor fallback | 1.040 | 1.033 | 1.041 | 1.036 |
| Lookup-hook fallback | 1.001 | 1.015 | 1.010 | 1.000 |
| Descriptor transition | 1.033 | 1.042 | 1.054 | 1.028 |
| Lookup-hook transition | 1.022 | 1.015 | 1.007 | 1.019 |

For the six sustained field callers, JIT process-elapsed ratios range from
0.706 to 0.788, CPU from 0.700 to 0.775, and RSS from 0.999 to 1.006.
Their workload times still range from 1.392 to 2.147 times CPython.

The field-call gains don't establish an application speedup. Focused
DeltaBlue/Richards/call-overhead/attribute JIT work ratios are
1.006/0.988/0.998/1.004; interpreter ratios are 0.996/1.007/0.984/1.000.
Initial JIT RSS ratios are 1.053/1.018/1.004/1.004. These memory observations
haven't been rechecked and remain open. Six simple scalar-call controls are
approximately flat at standard work; the sustained global-scalar interpreter
control costs 3.2%. The sustained default-scalar JIT ratio is 0.908, but that
isolated control result isn't an application claim.

The repeated descriptor costs motivate refinement before broad-suite and
startup admission. Those batches weren't run for this first candidate.
Original results aren't replaced by later measurements. Raw samples, hashes,
source snapshots, the complete patch, and immutable executable remain under
`target/performance/borrowed-field-arguments-*`.

## Early-cache-check refinement

The second candidate rejects a first field argument unless its primary cache
already names an instance dictionary or slot read. The full reader still checks
receiver identity, class versions, actual names, private storage, and all later
instructions. This check doesn't allocate cache metadata. A test-only correction
also excludes caller argument reads from the older callee-field coverage counts;
those counts match the accepted runtime again. Completed field-call counts and
all 38 compilation decisions/statistics remain unchanged.

All 389 VM tests, embedding, Clippy with existing exclusions, no-default
compilation, scoped formatting, and 14 benchmark-tool tests pass. The release
passes 291 runs across 97 fixtures and 768 semantic probes. The final build takes
7 minutes, 28 seconds, and its controller closes before freezing. The binary is
51,877,200 bytes with SHA-256
`5dcc66419b2389f8c40fb99fcdf38ffd568e630b2964cd7aaf3c7fa08f49fca7`;
its patch is
`660ecb81d54d2f2d0429394cdb84d42b54b1d2ec6b141ddb61bbfb395cd5064f`
against `3f73305`.

The sustained field-call JIT ratios range from 0.682 to 0.732 and interpreter
ratios from 0.651 to 0.726. However, descriptor fallback costs 2.5%/1.7%, lookup
hooks cost 2.7%/4.0%, and descriptor transitions cost 7.7%/5.0% in JIT/interpreter
work. Hook transitions measure 0.996/1.020. The two-argument scalar JIT control
costs 3.3%, repeating its standard-work result. Standard DeltaBlue/Richards work
ratios are 0.988/1.012 with JIT and 1.011/1.028 without it; their initial RSS
ratios are 1.005/1.021 and haven't been rechecked.

This refinement is also held. Neither variant establishes an application gain,
and broad/startup admission batches weren't run after repeated fallback costs.
Both complete patches and frozen executables remain under `target/`. The
committed behavioral fixture and reusable probes cover the opportunity and its
fallback costs; the accepted runtime still remains `6ea3e53`.
