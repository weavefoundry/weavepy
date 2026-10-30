"""A record class that also subclasses a `typing.Protocol` constructs with
its fields, and the callables stored in records are called hot.

`grpc._server._HandlerCallDetails` is a namedtuple subclass of the
`grpc.HandlerCallDetails` protocol. Its first construction runs
`typing._no_init_or_replace_init`, which stores `object.__init__` on the
class and calls it with the fields: CPython's `tp_init` is then still
`object_init`, which takes the arguments because the class overrides
`__new__`. Counting the stored `object.__init__` as an override raised
TypeError, and grpc's server answered every call with no response.

Each case runs in a fresh interpreter with every tier on, with a low
compile threshold, with a low frame-JIT threshold, with the method tier
off and with the compilers off (as test_override_fast_paths does)."""

import os
import subprocess
import sys
import unittest

CASES = {}

CASES["protocol_namedtuple"] = r'''
import collections
from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class Details(Protocol):
    method: str
    invocation_metadata: Any


class _Details(
    collections.namedtuple("_Details", ("method", "invocation_metadata")), Details
):
    pass


class Handler(Protocol):
    def unary_unary(self, request, context): ...


class _Handler(
    collections.namedtuple("_Handler", ("unary_unary", "request_streaming")), Handler
):
    pass


def lookup(details):
    if details.method == "/echo.Echo/Shout":
        return _Handler(lambda request, context: request.upper(), False)
    return None


def call(handler, request):
    return handler.unary_unary(request, None)


out = []
for i in range(400):
    d = _Details("/echo.Echo/Shout" if i % 3 else "/other", (("k", str(i)),))
    h = lookup(d)
    out.append(None if h is None else call(h, b"weavepy echo"))
print(isinstance(d, Details), _Details.__init__ is object.__init__, d.invocation_metadata)
print(out.count(b"WEAVEPY ECHO"), out.count(None))
'''

CASES["object_init_stored"] = r'''
class T(tuple):
    pass


T.__init__ = object.__init__


class P:
    __init__ = object.__init__


def errs(f):
    try:
        f()
        return "ok"
    except TypeError as e:
        return str(e)


rows = []
for i in range(300):
    rows.append((
        T((i, 1)),
        errs(lambda: P(i)),
        errs(lambda: object.__init__(T(()), i)),
        errs(lambda: P()),
    ))
print(rows[0], rows[-1], all(r[1:] == rows[0][1:] for r in rows))
'''

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    "protocol_namedtuple": "True True (('k', '399'),)\n266 134\n",
    "object_init_stored": "((0, 1), 'P() takes no arguments', 'ok', 'ok') "
    "((299, 1), 'P() takes no arguments', 'ok', 'ok') True\n",
}


def run_case(name, extra):
    env = dict(os.environ, **extra)
    result = subprocess.run(
        [sys.executable, "-c", CASES[name]],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class ProtocolRecordInitTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in CASES:
    setattr(ProtocolRecordInitTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
