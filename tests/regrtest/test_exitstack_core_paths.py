"""The steps `contextlib.ExitStack` takes on every use, run in the core
loop instead of as slow steps: a function's own dunder attribute store
(`_exit_wrapper.__wrapped__ = callback`), `callback(*args, **kwds)` of a
bound builtin method, `types.MethodType(cm_exit, cm)`, and
`sys.exception()`. Each case runs warm and checks the observable
results against what the general paths give.
"""

import contextlib
import sys
import types
import unittest

WARM = 2000


class Manager:
    def __init__(self, log):
        self.log = log

    def __enter__(self):
        self.log.append("enter")
        return self

    def __exit__(self, *exc):
        self.log.append(("exit", exc[0]))
        return False


class ExitStackCorePathTests(unittest.TestCase):
    def test_exitstack_warm(self):
        log = []
        for i in range(WARM):
            with contextlib.ExitStack() as stack:
                m = stack.enter_context(Manager(log))
                stack.callback(log.append, i)
                stack.callback(log.clear)
                self.assertIs(m.log, log)
            # (Callbacks run last in, first out.)
            self.assertEqual(log, [i, ("exit", None)])
            log.clear()

    def test_exitstack_exception(self):
        log = []
        for _ in range(WARM // 10):
            with self.assertRaises(KeyError):
                with contextlib.ExitStack() as stack:
                    stack.enter_context(Manager(log))
                    stack.callback(log.append, "cb")
                    raise KeyError
            self.assertEqual(log, ["enter", "cb", ("exit", KeyError)])
            log.clear()

    def test_function_dunder_attributes(self):
        def target():
            pass

        for i in range(WARM):
            def wrapper():
                pass

            wrapper.__wrapped__ = target
            wrapper.__isabstractmethod__ = bool(i & 1)
            wrapper.plain = i
        self.assertIs(wrapper.__wrapped__, target)
        self.assertEqual(
            wrapper.__dict__,
            {"__wrapped__": target, "__isabstractmethod__": True, "plain": WARM - 1},
        )
        for _ in range(WARM):
            wrapper.__doc__ = "doc"
            wrapper.__name__ = "renamed"
            wrapper.__qualname__ = "Q.renamed"
            wrapper.__module__ = "mod"
        self.assertEqual(
            (wrapper.__doc__, wrapper.__name__, wrapper.__qualname__, wrapper.__module__),
            ("doc", "renamed", "Q.renamed", "mod"),
        )
        self.assertNotIn("__doc__", wrapper.__dict__)
        self.assertNotIn("__name__", wrapper.__dict__)
        with self.assertRaises(TypeError):
            wrapper.__name__ = 1
        with self.assertRaises(TypeError):
            wrapper.__dict__ = 1
        with self.assertRaises(AttributeError):
            wrapper.__globals__ = {}
        d = {}
        wrapper.__dict__ = d
        wrapper.__wrapped__ = 5
        self.assertEqual(d, {"__wrapped__": 5})

    def test_call_ex_bound_builtin(self):
        log = [1, 2, 3]
        for i in range(WARM):
            args = (i,)
            kwds = {}
            app = log.append
            app(*args, **kwds)
            clear = log.clear
            if i % 100 == 49:
                clear(*(), **{})
        self.assertEqual(log, list(range(WARM - 50, WARM)))
        d = {}
        upd = d.update
        for i in range(WARM):
            upd(*({"k": i},), **{})
            upd(*(), **{"j": i})
        self.assertEqual(d, {"k": WARM - 1, "j": WARM - 1})
        with self.assertRaises(TypeError):
            log.append(*(1, 2), **{})
        with self.assertRaises(TypeError):
            log.append(*(1,), **{"x": 1})

    def test_method_type(self):
        class C:
            pass

        def f(self, x):
            return (self, x)

        obj = C()
        for i in range(WARM):
            m = types.MethodType(f, obj)
        self.assertIs(m.__self__, obj)
        self.assertIs(m.__func__, f)
        self.assertEqual(m(4), (obj, 4))
        with self.assertRaises(TypeError):
            types.MethodType(f)

    def test_sys_exception(self):
        self.assertIsNone(sys.exception())
        seen = []
        for i in range(WARM):
            try:
                raise ValueError(i)
            except ValueError:
                seen.append(sys.exception())
                self.assertIs(sys.exc_info()[1], seen[-1])
            self.assertIsNone(sys.exception())
        self.assertEqual(seen[-1].args, (WARM - 1,))

        def gen():
            try:
                raise KeyError("g")
            except KeyError:
                yield sys.exception()
                yield sys.exception()

        g = gen()
        first = next(g)
        self.assertIsNone(sys.exception())
        self.assertIs(next(g), first)


if __name__ == "__main__":
    unittest.main()
