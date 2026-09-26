# Union method binding and native SQLAlchemy

SQLAlchemy 2.1.1's compiled extension calls
`Union.__class_getitem__((int, str, "CacheConst"))` directly. WeavePy registered
this method as an unbound builtin, although its implementation expected the
class before the item argument. Normal `Union[...]` subscription supplied
that class itself, so ordinary Union tests passed while the extension import
failed with `__class_getitem__() missing argument`.

The method now uses the existing classmethod wrapper and requires exactly
one item after the bound class. Direct calls, saved bound methods, ordinary
subscription, and `PyObject_GetItem` agree. The regression includes nested
unions, duplicate members, `None`, forward references, `Literal`, and invalid
positional and keyword arguments.

## Validation

Built from `09a56e9`, the change passes 408 VM tests, embedding's explicit
1 MiB stack case, VM Clippy with the established exclusions, no-default
compilation, scoped formatting, and the repository artifact check.

The frozen CLI passes the new binding regression, native SQLAlchemy/Alembic
imports, six additional regression fixtures, and all 66 selected upstream
tests in each of JIT, interpreter, and GIL-disabled modes. The upstream
selection is `test_typing.UnionTests` plus the full `test_genericalias` module;
it also passes on the preceding binary and CPython 3.14.5. Native import
validation asserts that SQLAlchemy's `_util_cy._is_compiled()` is true.

Full native database probes remain unsuccessful. SQLAlchemy and Alembic now
reach compiled cache-key generation, where they fail with
`TypeError: 'object' object is not callable`. Both native probes pass CPython.
Both pure-Python package probes pass the preceding and new WeavePy binaries
in default JIT mode. This increment fixes the import blocker, without
claiming complete native SQLAlchemy compatibility.

## Startup and retained evidence

A 31-cycle alternating startup comparison against `09a56e9` discards warmup
and keeps normal collection enabled. JIT elapsed ratios for ordinary,
no-site, isolated, and import startup are 1.002, 0.991, 0.985, and 0.994;
corresponding CPU ratios are 1.014, 1.015, 0.991, and 1.001. JIT RSS ratios
range from 1.001 to 1.009. Interpreter elapsed ratios range from 0.942 to
0.981, CPU from 0.936 to 0.986, and RSS from 0.994 to 1.000. This is a
compatibility fix; these single-batch movements aren't a startup speedup
claim. Ordinary JIT startup still takes 1.648 times CPython elapsed.

All owned builds and correctness checks finished before timing. The shared
Intel macOS desktop had active Chrome and Codex renderers, with their CPU
usage recorded. No benchmark, baseline, gate, JIT budget, or package source
was weakened or changed.

The executable remains 51,894,008 bytes. Its SHA-256 is
`874a63e9add85651c0bb321eca4631bb11fb9b725e1d2a0372265e45f48ec830`;
the runtime patch SHA-256 is
`610c28635b22a8985db66de6abbed9c2db6d578d7bfdebdba092e3da1dbb84cf`.
The locally built native SQLAlchemy wheel has SHA-256
`bf9d2569da491a6185dbae052ee88b166d64c22350103b62420914b5da6b5f7b`.
Sources, package inputs, binaries, test logs, and measurements remain under
`target/performance/sqlalchemy-import-investigation/` and
`target/performance/weavepy-union-classmethod`.
