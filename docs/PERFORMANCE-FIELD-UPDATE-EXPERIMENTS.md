# Scalar field-update experiments

Three unadopted revisions evaluate a method that adds an exact integer to one
instance field and returns that field. They recognize the precise bytecode
shape, check binding, code, defaults, recursion, observers, GIL state, field
caches, and overflow, and perform one final store. They don't classify mutation
as a read-only leaf or bypass a compiled callee from the interpreter.

The first revision reduces interpreter Richards work by about half but raises
slot-backed interpreter work by 20%, repeated at sustained work. An early store
cache guard removes that large slot cost in the second revision, which still
raises class-default and mutable-class-value work by 29–32% and large-integer
work by 9–12%. Both revisions are held.

The third revision records a conservative class-value fact in the existing
16-byte store cache. Absent values and selected exact built-in types can't
acquire descriptor hooks independently of the receiver class version. Mutable
class values remain excluded. The fact survives new-key upgrades only while
the class version holds. An early integer-tag check avoids preparing arguments
for large-integer fields; the final evaluator retains all guards.

## Results and decision

Measurements use the Intel macOS host, normal release settings, optimized
CPython 3.14.5, and accepted runtime `4377ce1` at documentation-only `b65dad3`.
Seven interleaved paired cycles follow discarded warmup and frozen-cache
verification. No build, test, or profiling work overlaps timing. Ratios are
candidate/accepted; lower is better.

| Third revision | Standard JIT | Standard interpreter | Sustained JIT | Sustained interpreter |
| --- | ---: | ---: | ---: | ---: |
| Richards fixture | 1.003 | 0.441 | 0.995 | 0.438 |
| Mixed-call fixture | 0.911 | 0.916 | 0.904 | 0.907 |
| Mutable class value | 1.059 | 1.044 | 1.064 | 1.052 |
| Large-integer field | 1.014 | 1.037 | 1.024 | 1.032 |
| Original slot control | 1.003 | 1.038 | 0.989 | 1.021 |

Sustained calls use one million updates; Richards and mixed calls use five
times their standard work. The third revision remains held because fallback
costs repeat. Sustained Richards and mixed-call JIT work still take 3.240 and
2.538 times CPython, respectively. No broad-suite, startup, or memory gain is
established. The full controls and startup batches weren't run for these held
revisions. Sustained preflight also found simulator and desktop background
activity, so small differences remain provisional; all samples are retained.

An expanded ten-mode probe inadvertently changed loop compilation: a
constructor-type dictionary made its constant/default loops reject with
`UnsupportedConst` in both binaries. Its approximately 60% default-JIT gains
therefore don't describe the original compiled loops. Repeating the unchanged
six-mode source gives constant/default JIT ratios of 0.977/0.992 and interpreter
ratios of 0.385/0.369. The reusable tools preserve the original body in
`tools/bench_field_updates.py` and the four additional cases separately in
`tools/bench_field_update_fallbacks.py`. Both use `WEAVEPY_FIELD_UPDATE_KIND`,
which the comparison harness records.

## Validation and provenance

The third candidate passes 391 VM tests, compiler encoding/layout checks,
embedding, Clippy with the established VM exclusions, no-default compilation,
formatting, and 14 benchmark-tool tests. Its frozen release passes 297 runs
across 99 fixtures, 1,048 semantic probes, twelve CPython fixtures, seven
constant/shape checks, and 60 additional fallback checks. All 57 initial probe
and three application compilation decisions match the accepted binary. The
original probe adds 180 semantic checks and six matching compilation traces.

Completed-path counters cover parameters, constants, defaults, and class
constants, with 3,998 updates per 4,000-call interpreter probe and 689 with JIT.
The six unsupported controls record no evaluator attempts. Fixtures cover
integer overflow, retained dictionaries and iterators, dictionary reshaping,
code/default/binding changes, inherited attributes, MRO changes, callback
frames, descriptor order, arithmetic errors, and trace/profile events. A
separate mutable descriptor-class invalidation defect also reproduces in the
accepted runtime; these experiments don't fix it.

The third CLI build takes 7 minutes, 37 seconds and is frozen after its build
controller closes. It is 51,877,536 bytes, SHA-256
`c581659200402f1a11eaf59bc1f37676a6679f4db4b32d6c2207f00380e6033c`.
Its patch against `b65dad3` is
`c4379fc0ef1ae6d847364c3932d791f650eb622bd84310e5abf1e4314c111f00`.
Exact sources, earlier candidates, raw results, traces, and binaries remain
under `target/performance/`. No runtime optimization from this report is adopted.
