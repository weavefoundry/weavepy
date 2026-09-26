# C iterator slots for native lazy adapters

The native SQLAlchemy 2.1.1 probe reached its async query after the Union
and class-descriptor fixes, but returned no rows despite successful
insertion. Pure-Python SQLAlchemy passed, and native `.all()` calls
returned the expected rows. Native result iteration didn't.

A smaller `ChunkedIteratorResult` reproduced the difference without a
database or coroutine. Its iterator is an `itertools.chain.from_iterable`
adapter. WeavePy represents the adapter's iterator as `Object::LazyIter`,
which the C wrapper classified as plain `object`. The resulting C type
had no `tp_iter` or `tp_iternext` slot. Cython reads those slots directly;
exercising only the `PyIter_Next` function wouldn't catch this error.

The C type mapping now sends `Object::LazyIter` through the existing
`PySeqIter_Type`, alongside `Object::Iter`. That type already implements
self iteration, exhaustion, and exception propagation. The iteration
bridge itself is unchanged, including its preservation of generator
return values in `StopIteration.value`.

## Validation

The new Rust integration test fails on the preceding runtime and passes
with this mapping. It reads and calls both C slots directly and checks
list iterators, chain, islice, repeat, zip-longest, generators, empty
adapters, repeated exhaustion, owned self references, `ValueError`, and
a generator's return value. It doesn't depend on `ctypes` or an external
C compiler.

A separate extension compiled against stock CPython 3.14 headers checks
six iterator families. Before this change, CPython passed all six while
WeavePy failed the four lazy adapters. The frozen candidate passes all
six under JIT, interpreter, and GIL-disabled launches, as does CPython.

The candidate passes all 34 C API unit tests and 45 selected integration
tests. The native stock-ABI, stock-type, Cython, and numpy-like libraries
were present; their integration assertions ran. Embedding's explicit
1 MiB stack test, Clippy with the established exclusions, and no-default
compilation also pass.

All 40 frozen-release validation runs pass. They include the full native
SQLAlchemy Core, synchronous ORM, and async query probe; native Alembic
migration, autogeneration, upgrade, and downgrade; the stock-header
iterator oracle; and the preceding descriptor/Union regression checks.
Six regression fixtures pass in three WeavePy launch modes. Eight
selected upstream descriptor tests pass per mode and two skip unavailable
`xxsubtype`; CPython passes all ten. No ignored exceptions appear.
Native modules that haven't declared free-threading support enable the
GIL when loaded, including in the GIL-disabled launches. These checks
therefore don't establish free-threaded native SQLAlchemy execution.

Windows CI for `f0117c8` passes the direct descriptor integration test,
clearing its earlier `_ctypes.COMError` import blocker. That commit's
formatting check reports an import-order disagreement with the local
formatter. Qualifying the two private API calls removes that ambiguity;
the final direct test and its Clippy check pass locally. This test-only
qualification was made after the release build and doesn't alter its
runtime. Windows CI for the iterator fix remains pending.

## Startup controls and retained costs

Two independent 31-cycle comparisons against the unchanged `6343cc1`
runtime alternate process order and discard warmup. Setup and normal
collection stay timed. Owned builds, tests, and diagnostic controllers
finished before measurements. This is a shared Intel macOS desktop:
`mediaanalysisd` used about 100% of one CPU in both pre-run snapshots,
and Codex renderer activity was also present. That background activity
doesn't justify discarding observed costs.

Initial JIT elapsed ratios for ordinary, no-site, isolated, and import
startup are 1.009, 1.011, 1.015, and 1.014; CPU ratios are 1.027,
1.062, 1.024, and 1.020. Repeated elapsed ratios are 1.000, 1.034,
1.008, and 1.010; CPU ratios are 1.018, 1.056, 1.016, and 1.006.
The no-site CPU cost remains, and its repeated elapsed cost is 3.4%.
Repeated JIT RSS ratios are 0.998-1.005. Interpreter elapsed ratios
are 0.961-0.994 initially and 0.967-0.993 on repetition, with repeated
CPU ratios of 0.959-0.991 and RSS of 0.995-0.998.

This compatibility fix doesn't establish general performance gains.
Ordinary JIT startup still takes 1.683 times CPython elapsed on
repetition. The complete application benchmark suite wasn't repeated
for this C type mapping change, and the database probe checks establish
correctness, not a database speedup.

## Source identity and retained evidence

The candidate is based on `f0117c8`, whose runtime is identical to
`6343cc1`. Its executable is 51,894,312 bytes, unchanged from that runtime.
Its SHA-256 is
`931b094f51ace6f580e87ef865047fb33a0d5eabccba43023f419ee6183f4ff0`;
the iterator runtime patch SHA-256 is
`6cf8db3f7059ed257396a12a8aff2bcc73157a1cdfa61fd80d6ff3c5853db48d`.
The native SQLAlchemy wheel and package inputs are unchanged from the
[Union binding investigation](UNION-METHOD-COMPATIBILITY.md).

Sources, build inputs, native-library identities, validation logs, and
measurements remain under
`target/performance/capi-lazy-iterator-investigation/`. The frozen binary
is `target/performance/weavepy-capi-lazy-iterators`. The earlier isolated
ORM and stock-header reproductions remain under
`target/performance/capi-class-descriptor-investigation/`.
