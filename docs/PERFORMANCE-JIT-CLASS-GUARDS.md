# Release classes retained by native version guards

A compiled attribute or method guard used to own the class from which it was
learned, even after every instance and Python class reference had disappeared.
Keeping the reader function alive therefore kept the old class alive too.
The guards now retain their globally unique class-version tokens without an
unused strong class reference. Attribute fingerprinting also avoids the
corresponding reference-count operations.

Version tokens never repeat, including after class destruction or mutation.
Each live receiver owns the class whose version the access helper checks.
Names, storage indices, value lanes, instance shadowing, descriptors, and method
code-identity checks remain intact. Method guards still own their function and
code. Other constructor, global, and callee owners are outside this change;
functions that themselves refer to a class can still retain it.

## Measurement

The exact baseline is 83c34d7. The rejected native-method fusion experiments
aren't included in either binary. Paired runs alternate order, discard a warmup
cycle, use separate frozen caches, and record workload time, process elapsed,
process CPU, and peak RSS. Builds and correctness checks finish before timing.
The host is macOS x86-64, with optimized CPython 3.14.5 as the reference.

The tools/bench_jit_class_guards.py probe retains 256 independent compiled
readers after their temporary classes and instances become unreachable. It
collects every 16 classes, verifies the reader's result, and checks replacement
classes through the retained readers. Each reader performs 10,000 additions.
The two payload sizes are measured separately over seven paired cycles.

| Class payload | JIT workload/baseline | JIT elapsed/baseline | JIT CPU/baseline | JIT RSS/baseline |
| --- | ---: | ---: | ---: | ---: |
| None | 1.012 | 1.012 | 1.002 | 0.994 |
| 64 KiB | 0.985 | 1.003 | 1.009 | 0.715 |

The payload case's marginal median RSS falls from 39.32 MB to 28.04 MB.
The 28.5% paired reduction is specific to this retained-data case; ordinary
classes show little peak-memory change. Against CPython, the modified runtime
still takes 1.698 times the ordinary probe's workload time and 1.794 times its
RSS. The payload case takes 2.134 times CPython's time and 1.526 times its RSS.
Ratios are medians of paired ratios, so dividing marginal medians can differ.

The three-cycle standard suite gives these geometric means. Workload time
covers 23 fixtures; process metrics cover all 24, including startup.

| Metric | JIT/baseline | Interpreter/baseline | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 0.992 | 1.006 | 1.086 |
| Process elapsed | 0.992 | 0.999 | 1.321 |
| Process CPU | 0.995 | 1.000 | 1.300 |
| Peak RSS | 1.000 | 0.993 | 1.613 |

Seven-cycle rechecks do not reproduce the initial attribute-access JIT time,
list-operation RSS, or interpreter JIT-loop increases: their repeated ratios
are 1.006, 0.987, and 0.980. Interpreter Richards retains a smaller 2.5% time
increase, down from 5.0% in the initial three-cycle pass. Call-overhead JIT time
improves in the focused, full, and repeated runs; the repeated ratio is 0.974.

Two 31-cycle startup runs leave normal, isolated, and import-batch JIT elapsed
time within 1% of baseline. Without site, elapsed ratios are 1.017 and 1.025;
CPU ratios are 1.041 and 1.025. The repeated interpreter-only elapsed ratios
are 0.983, 0.967, 0.981, and 0.988 for normal, no-site, isolated, and imports,
respectively. The no-site JIT increase and Richards interpreter increase
remain limitations. This change doesn't establish universal CPython parity.

## Validation

The regression independently exercises dictionary reads, slotted reads,
stores, and method calls. Weakrefs and callbacks verify class and instance
collection while readers remain live. The method callee returns a constant,
so that case isolates method ownership from attribute guards in the callee.
Its loop stays below the native call-density retirement poll. The Rust test
runs in an isolated process and verifies all four retained drivers are still
compiled after collection, before checking fresh layouts, descriptors, and
method replacements in the same globals.

The accepted baseline retains all four classes in an independent trace while
releasing their instances. The candidate passes the focused test and all 366
VM tests with RUST_MIN_STACK=8388608. All 41 selected release regression scripts
pass with the JIT, without the JIT, and in free-threaded mode (123 runs). The final
fixture also passes on CPython 3.14.5. Clippy
passes with the two existing exclusions used by preceding reports.

## Reproduction

Build the CLI with cargo build --release -p weavepy-cli --bin weavepy and save
the baseline binary before building the candidate. Use tools/bench_compare.py
with --probe tools/bench_jit_class_guards.py --work 256 --samples 7, setting
WEAVEPY_CLASS_GUARD_PAYLOAD separately to 0 and 65536. Set WEAVEPY_STDLIB_CACHE
to a staged standard library and supply a fresh --frozen-cache-root for each
comparison. The regular suite uses its existing fixtures and work sizes;
tools/bench_startup.py measures the four startup cases over 31 cycles.
