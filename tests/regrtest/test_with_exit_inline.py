"""A `with` block's exit on an exception: when the manager's `__exit__` is a
plain Python function (not a frameless leaf), `WITH_EXCEPT_START` runs it as
an inline activation, as a `CALL` of the same operands would (see
`core_with_exit_inline` in the VM), instead of nesting the call.

Each case runs warm (loops long enough to compile the callers) and checks
what the nested call did: the arguments, the handled exception, the
caller's frame, suppression and re-raising, raises out of `__exit__` with
their context and traceback, and finalizer timing.
"""

import sys
import traceback
import unittest

WARM = 3000


class Recorder:
    """An `__exit__` too large to be a frameless leaf."""

    def __init__(self, suppress):
        self.suppress = suppress
        self.seen = []

    def __enter__(self):
        return self

    def __exit__(self, exctype, excinst, exctb):
        if exctype is None:
            return None
        self.seen.append((exctype, excinst, exctb is excinst.__traceback__))
        if issubclass(exctype, BaseExceptionGroup):
            match, rest = excinst.split(KeyError)
            if rest is None:
                return True
            raise rest
        return self.suppress


class Varargs:
    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.args = args
        try:
            pass
        finally:
            self.done = True
        return True


class Inspector:
    def __enter__(self):
        return self

    def __exit__(self, exctype, excinst, exctb):
        caller = sys._getframe(1)
        self.line = caller.f_lineno
        self.name = caller.f_code.co_name
        self.info = sys.exc_info()[1]
        for _ in ():
            raise
        return True


class Raiser:
    def __enter__(self):
        return self

    def __exit__(self, exctype, excinst, exctb):
        if exctype is not None:
            raise ValueError("from exit") from None
        return False


class Chained:
    def __enter__(self):
        return self

    def __exit__(self, exctype, excinst, exctb):
        if exctype is not None:
            raise ValueError("from exit")
        return False


class Mortal:
    log = []

    def __del__(self):
        Mortal.log.append("del")


class Holder:
    def __enter__(self):
        return self

    def __exit__(self, exctype, excinst, exctb):
        self.held = excinst
        for _ in ():
            raise
        return True


class WithExitInlineTests(unittest.TestCase):
    def test_suppress_warm(self):
        r = Recorder(True)
        d = {}
        for i in range(WARM):
            with r:
                d[i]
        self.assertEqual(len(r.seen), WARM)
        t, e, same_tb = r.seen[-1]
        self.assertIs(t, KeyError)
        self.assertIsInstance(e, KeyError)
        self.assertEqual(e.args, (WARM - 1,))
        self.assertTrue(same_tb)

    def test_reraise_warm(self):
        r = Recorder(False)
        caught = 0
        for i in range(WARM):
            try:
                with r:
                    raise IndexError(i)
            except IndexError as e:
                self.assertEqual(e.args, (i,))
                caught += 1
        self.assertEqual(caught, WARM)

    def test_reraise_keeps_traceback(self):
        r = Recorder(False)

        def body():
            with r:
                raise IndexError("deep")

        for _ in range(WARM):
            try:
                body()
            except IndexError as e:
                tb = e.__traceback__
        names = [f.name for f in traceback.extract_tb(tb)]
        self.assertEqual(names[-1], "body")
        self.assertIn("raise IndexError", traceback.format_tb(tb)[-1])

    def test_group_split(self):
        r = Recorder(False)
        for _ in range(WARM):
            with r:
                raise ExceptionGroup("g", [KeyError(1)])
        with self.assertRaises(ExceptionGroup) as cm:
            with r:
                raise ExceptionGroup("g", [KeyError(1), ValueError(2)])
        self.assertEqual(len(cm.exception.exceptions), 1)
        self.assertIsInstance(cm.exception.exceptions[0], ValueError)

    def test_varargs_exit(self):
        v = Varargs()
        for i in range(WARM):
            with v:
                raise KeyError(i)
        self.assertEqual(len(v.args), 3)
        self.assertIs(v.args[0], KeyError)
        self.assertEqual(v.args[1].args, (WARM - 1,))
        self.assertIs(v.args[2], v.args[1].__traceback__)
        self.assertTrue(v.done)

    def test_caller_frame_and_exc_info(self):
        insp = Inspector()
        for i in range(WARM):
            with insp:
                expected = sys._getframe().f_lineno - 1
                raise KeyError(i)
        self.assertEqual(insp.line, expected)
        self.assertEqual(insp.name, "test_caller_frame_and_exc_info")
        self.assertIsInstance(insp.info, KeyError)
        self.assertEqual(insp.info.args, (WARM - 1,))
        self.assertIsNone(sys.exc_info()[1])

    def test_raise_from_exit_chains_context(self):
        c = Chained()
        for i in range(WARM):
            try:
                with c:
                    raise KeyError(i)
            except ValueError as e:
                err = e
        self.assertEqual(err.args, ("from exit",))
        self.assertIsInstance(err.__context__, KeyError)
        self.assertEqual(err.__context__.args, (WARM - 1,))
        names = [f.name for f in traceback.extract_tb(err.__traceback__)]
        self.assertEqual(names[-1], "__exit__")
        self.assertIn("test_raise_from_exit_chains_context", names)

    def test_raise_from_exit_inside_handler(self):
        c = Raiser()
        hits = 0
        for i in range(WARM):
            try:
                with c:
                    raise KeyError(i)
            except ValueError:
                hits += 1
        self.assertEqual(hits, WARM)
        self.assertIsNone(sys.exc_info()[1])

    def test_exception_released_promptly(self):
        h = Holder()
        for _ in range(WARM):
            with h:
                raise KeyError(Mortal())
        Mortal.log.clear()
        h.held = None
        self.assertEqual(Mortal.log, ["del"])

    def test_nested_withs(self):
        outer = Recorder(True)
        inner = Recorder(False)
        for i in range(WARM):
            with outer:
                with inner:
                    raise LookupError(i)
        self.assertEqual(len(inner.seen), WARM)
        self.assertEqual(len(outer.seen), WARM)
        self.assertIs(inner.seen[-1][1], outer.seen[-1][1])

    def test_recursion_limit(self):
        r = Recorder(True)

        def deep(n):
            with r:
                if n:
                    deep(n - 1)
                raise KeyError(n)

        old = sys.getrecursionlimit()
        try:
            sys.setrecursionlimit(200)
            for _ in range(50):
                try:
                    deep(1000)
                except RecursionError:
                    pass
        finally:
            sys.setrecursionlimit(old)
        deep(5)


if __name__ == "__main__":
    unittest.main()
