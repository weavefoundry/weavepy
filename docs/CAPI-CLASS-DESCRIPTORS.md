# C API class descriptor lookup

Native SQLAlchemy 2.1.1 calls a custom descriptor on its `_MetaOptions`
class while generating cache keys. WeavePy's fast C attribute lookup
returned that descriptor unchanged. Calling the returned object failed
with `TypeError: 'object' object is not callable`.

Class lookups now defer Python descriptors to the existing VM attribute
protocol, which calls `__get__(None, owner)`. The two legacy optional
attribute helpers also use the full protocol when a fast lookup declines.
They distinguish missing attributes from other descriptor errors and
preserve owned-result handling.

The regression exercises both public attribute spellings, optional
lookups, inherited owners, a custom metaclass, callable results, runtime
descriptor mutation, and exception propagation. A dependency-free
descriptor oracle passes CPython and fails the accepted `a7a14bf` runtime.

## Validation

Built from `e98b245`, this change passes 34 C API unit tests and 44 selected
integration tests, including embedding lifecycle and the stock ABI, type,
Cython, and NumPy-like fixtures. The native fixture libraries exist and
load successfully; these tests didn't silently skip missing extensions.
C API Clippy passes with the established exclusions, as does compilation
without default features.

The frozen CLI passes six regression fixtures in each of JIT, interpreter,
and GIL-disabled launch modes: the new C attribute regression, Union
binding, object-model edges, C recursion, the greenlet C capsule, and
drop-in compatibility. Native extensions that haven't declared free-thread
safety can enable the GIL during their tests. The selected upstream
descriptor group has eight passes and two `xxsubtype` skips per WeavePy
mode; all ten pass CPython 3.14.5. The C attribute and Union fixtures also pass
CPython. No ignored exceptions appear in these logs.

Native SQLAlchemy and Alembic imports pass in all three WeavePy modes,
with `_util_cy._is_compiled()` asserted. Alembic's full SQLite migration
probe now passes all three modes. SQLAlchemy's Core and synchronous ORM
sections pass, but its async result assertion still fails with an empty
list instead of one row. Both complete native probes pass CPython. An
independent diagnostic confirms that the expected rows were inserted;
the pure-Python package passes that diagnostic. Basic native result
iteration also passes, so the remaining issue isn't isolated to general
row iteration. Complete native SQLAlchemy compatibility isn't claimed.

## Startup and retained evidence

Two 31-cycle alternating comparisons against `e98b245` discard warmup,
retain normal collection, and measure elapsed time, CPU, and peak RSS.
All owned builds, tests, and diagnostics finished before timing. Background
Chrome and Codex renderers and metadata indexing were active on this shared
Intel macOS desktop; it wasn't an idle benchmark host.

Initial JIT elapsed ratios for ordinary, no-site, isolated, and import
startup are 1.011, 1.034, 1.000, and 1.009; CPU ratios are 1.033, 1.056,
1.012, and 1.011. Repeated elapsed ratios are 0.991, 0.993, 0.997, and
0.997, with CPU ratios of 0.990, 1.026, 0.996, and 0.996. The initial
costs above 3% don't repeat. Repeated JIT RSS ratios range from 0.998 to
1.013. Interpreter elapsed ratios range from 0.928 to 0.982 initially
and 0.974 to 0.987 on repetition; repeated CPU ratios range from 0.950
to 0.982. These controls don't establish a general startup improvement
from this compatibility fix. Ordinary JIT startup still takes 1.676 times
CPython elapsed on repetition.

The executable is 51,894,136 bytes, 32 bytes smaller than its predecessor.
Its SHA-256 is
`6bf59c9373156e8e07bbcbeb9ced131a5848d243aecffb942dd89034762d794e`;
the runtime patch SHA-256 is
`0aecb9f9c029d968c1a3530abbdc4ec288457bd2267b5b6ffcb6409004051db4`.
The native SQLAlchemy wheel is unchanged from the
[Union binding investigation](UNION-METHOD-COMPATIBILITY.md).
Sources, package inputs, test logs, and measurements remain under
`target/performance/capi-class-descriptor-investigation/`, with the frozen
binary at `target/performance/weavepy-capi-class-descriptors`.

## Portable integration test follow-up

Windows CI for `b1c903d` failed before the new regression reached any
descriptor assertions: importing `ctypes` requires `_ctypes.COMError`,
which WeavePy doesn't yet provide on Windows. The Rust integration test
now initializes the embedded interpreter and calls the C attribute APIs
directly. It checks inherited descriptor owners, callable results,
descriptor mutation, error values, optional missing results, and owned
references. It also exercises both private lookup helpers and their
existing presence-only behavior. The independent Python/CPython fixture
remains unchanged.

The direct integration test and its Clippy check pass locally. Windows
execution remains pending CI; this test-only change doesn't implement
Windows COM support or change the runtime measured above. Its logs are
under `target/performance/capi-direct-descriptor-test/`.
