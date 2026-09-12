# Rejected lazy finalization metadata experiment

The candidate is rejected. Ordinary retained heaps use about 2 percent less peak memory, but finalizer heaps use about 4.2 percent more in both modes. Every finalizer RSS sample exceeds every corresponding baseline sample. Callback-heavy collection also regresses. The runtime change will be removed; the independent weak-reference regression remains useful.

The baseline is validated release `36989234372e807ab95dc79686d9e5dd3bb51b458a9f7a1d92992357c30373a0` (44,289,312 bytes), containing the preceding borrowed-handle change. That preceding change has not yet received its own performance comparison. The rejected candidate is `75702d77194b76d049d7fab76e4ebb66ed01d76f64a1423e10d3d8324347cfea` (44,290,224 bytes, an increase of 912). Exact sources, inputs, and release identities are retained.

The candidate passes 344 VM tests, formatting, Clippy, the no-default-features check, 30 targeted JIT/interpreter/GIL-disabled checks, and all 275 compatibility checks. All four measured heaps pass before/after-warmup root and watcher checks on both releases and CPython, plus candidate GIL-disabled checks. Correctness alone does not make this a performance improvement.

## Uninstrumented measurements

Each case retains 100,000 nodes. The work timer covers five full collections; process wall time, CPU, and peak RSS also include setup and shutdown. Seven paired cycles follow one warm cycle, with alternating variant order and separate unchanged frozen-code caches per release. Every sample is retained. The load gate required three consecutive ten-second observations with both one- and five-minute load at most four on eight logical CPUs. During the command, observed one-minute load ranged from 2.881 to 4.642. Load telemetry does not establish an exclusively idle host.

Ratios below one favor the candidate. Work CPU is scoped to the collection timer; process CPU is scoped to the entire child. CPython is the installed GIL build.

| Heap | Mode | Work/before | Work CPU/before | Wall/before | CPU/before | RSS/before | Work/CPython | RSS/CPython |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| ordinary_100000 | jit | 0.9814 | 0.9817 | 0.9904 | 0.9922 | 0.9790 | 7.6710 | 2.7827 |
| ordinary_100000 | interp | 0.9828 | 0.9827 | 0.9782 | 0.9806 | 0.9758 | 7.5908 | 2.6876 |
| finalizer_100000 | jit | 0.9933 | 0.9936 | 1.0091 | 1.0092 | 1.0420 | 7.6358 | 3.1027 |
| finalizer_100000 | interp | 1.0111 | 1.0109 | 1.0128 | 1.0120 | 1.0419 | 7.4729 | 3.0031 |
| weak_no_callback_100000 | jit | 0.9727 | 0.9725 | 0.9799 | 0.9799 | 0.9934 | 7.7372 | 7.2580 |
| weak_no_callback_100000 | interp | 1.0294 | 1.0280 | 1.0218 | 1.0214 | 0.9939 | 7.7019 | 7.1934 |
| weak_callback_100000 | jit | 1.1290 | 1.1245 | 1.1162 | 1.1117 | 1.0098 | 21.7848 | 7.8972 |
| weak_callback_100000 | interp | 1.0489 | 1.0498 | 1.0553 | 1.0571 | 1.0056 | 21.0155 | 7.7941 |

## Allocation mechanism

The core TrackedHandle payload shrinks from 80 to 64 bytes. Its ordinary Arc allocation requests 96 bytes before and 80 after. Lazy finalization metadata has a 32-byte payload and requests a separate 48-byte Arc allocation. Release-library layout constants, unit assertions, exact allocator call sites, and malloc stub bindings establish these sizes.

Both releases have matching live profiles for zero and 3,000 retained nodes. The exact direct GC tracking sites add 3,000 handles for ordinary, finalizer, and noncallback weakref heaps, and 6,000 for callback heaps. Auxiliary metadata adds no allocations for ordinary or noncallback heaps, but adds 3,000 for each finalizer/callback heap. At these sites, normal requested bytes therefore decrease by 48,000 for ordinary/noncallback heaps, increase by 96,000 for finalizer heaps, and increase by 48,000 for callback heaps. These counts describe selected live allocation sites, not total heap or allocation churn.

A separate C control demonstrates instrumentation effects: malloc_history reports 96 bytes for a request of 80, 112 for a request of 96, and 64 for a request of 48. Uninstrumented malloc_size returns 80, 96, and 48 respectively. The profile parser retains reported bytes and inferred requested bytes as separate fields, checks every raw-history checksum, and classifies only exact symbol/return-offset matches. Virtual stack mappings and unclassified allocation groups are not summed as RSS.

## Limits and disposition

No full census or startup comparison was run for this rejected candidate. That decision experiment was declared before timing began. No claim is made for energy, controlled build latency, free-threaded CPython throughput, or universal performance superiority. The latest complete full census remains the preceding gc-traversal-lists archive. WeavePy still uses more time and memory than CPython on every heap in this experiment.

All measurements, validation, source snapshots, disassembly, live histories, and the instrumentation control are preserved here. Frozen artifacts are losslessly compressed. Generated binaries, debug-symbol bundles, bytecode caches, and ephemeral stack-log indexes are omitted with their paths recorded; executable hashes remain in the reports.
