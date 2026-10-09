"""Exceptions caught by handlers in compiled code.

A raise from a helper of compiled code (a dict's missing key, a call, a
`raise`) goes to the running function's handler without leaving native
code, and the handler's `PUSH_EXC_INFO`, `CHECK_EXC_MATCH`, `POP_EXCEPT`
and the `as` name's `DELETE_FAST` run there too. Everything an
interpreted handler shows must hold: the bound name and its unbinding,
`sys.exc_info()`, chaining, tracebacks, `finally`, a handler that doesn't
match, nesting, and generators.
"""

import os
import subprocess
import sys
import unittest

FORCED = "--forced-frame-jit"
N = 3000


def missing_keys(d, n):
    hits = misses = 0
    for i in range(n):
        try:
            hits += d[i]
        except KeyError:
            misses += 1
    return hits, misses


def bound_name(d, n):
    keys = []
    for i in range(n):
        try:
            d[i]
        except KeyError as e:
            keys.append(e.args[0])
            info = sys.exc_info()[1]
            assert info is e
    assert sys.exc_info() == (None, None, None)
    try:
        e
    except NameError:
        pass
    else:
        raise AssertionError("the as name stayed bound")
    return keys


def tuple_keys(d, n):
    misses = 0
    for i in range(n):
        try:
            d[(i, i % 3)]
        except KeyError as e:
            assert e.args[0] == (i, i % 3)
            misses += 1
    return misses


def nested(d, n):
    out = []
    for i in range(n):
        try:
            try:
                d[i]
            except KeyError:
                out.append("inner")
                d["boom"]
        except KeyError as outer:
            out.append(outer.args[0])
            assert isinstance(outer.__context__, KeyError)
    return out


def no_match(d, n):
    for i in range(n):
        try:
            d[i]
        except IndexError:
            return "wrong"
    return "done"


def with_finally(d, n):
    log = []
    for i in range(n):
        try:
            try:
                d[i]
            finally:
                log.append(i)
        except KeyError:
            pass
    return log


def raise_line(d):
    for i in range(N):
        try:
            d[i]
        except KeyError as e:
            last = e
    return last.__traceback__.tb_lineno


class AppError(Exception):
    pass


def raise_from(d, n):
    seen = []
    for i in range(n):
        try:
            try:
                d[i]
            except KeyError as e:
                if i % 2:
                    raise AppError(i) from None
                raise AppError(i) from e
        except AppError as err:
            seen.append((err.args[0], err.__cause__ is None, err.__suppress_context__,
                         type(err.__context__).__name__))
    return seen


def raise_class(n):
    count = 0
    for i in range(n):
        try:
            raise ValueError
        except ValueError as e:
            assert type(e) is ValueError and e.args == ()
            count += 1
    return count


def gen(d, n):
    for i in range(n):
        try:
            yield d[i]
        except KeyError:
            yield -1


class FrameJitExceptionsTest(unittest.TestCase):
    def test_missing_keys(self):
        d = {i: 1 for i in range(0, N, 2)}
        self.assertEqual(missing_keys(d, N), (N // 2, N // 2))

    def test_bound_name(self):
        self.assertEqual(bound_name({}, N), list(range(N)))

    def test_tuple_keys(self):
        d = {(i, i % 3): i for i in range(0, N, 2)}
        self.assertEqual(tuple_keys(d, N), N // 2)

    def test_nested(self):
        self.assertEqual(nested({}, N), ["inner", "boom"] * N)

    def test_no_match(self):
        with self.assertRaises(KeyError) as cm:
            no_match({0: 0, 1: 1}, N)
        self.assertEqual(cm.exception.args, (2,))

    def test_finally(self):
        self.assertEqual(with_finally({}, N), list(range(N)))

    def test_traceback_line(self):
        line = raise_line({})
        self.assertEqual(line, raise_line.__code__.co_firstlineno + 3)

    def test_raise_from(self):
        seen = raise_from({}, N)
        self.assertEqual(seen[0], (0, False, True, "KeyError"))
        self.assertEqual(seen[1], (1, True, True, "KeyError"))
        self.assertEqual(len(seen), N)

    def test_raise_class(self):
        self.assertEqual(raise_class(N), N)

    def test_generator(self):
        d = {i: i for i in range(0, N, 2)}
        self.assertEqual(list(gen(d, N)), [i if i % 2 == 0 else -1 for i in range(N)])

    def test_forced_frame_jit(self):
        if FORCED in sys.argv:
            self.skipTest("already forced")
        env = dict(os.environ)
        env.update(
            WEAVEPY_JIT="0",
            WEAVEPY_FRAME_JIT_TUNE="8,1,3,0",
            WEAVEPY_FRAME_JIT_HOT="50",
        )
        r = subprocess.run(
            [sys.executable, __file__, FORCED],
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main(argv=[a for a in sys.argv if a != FORCED])
