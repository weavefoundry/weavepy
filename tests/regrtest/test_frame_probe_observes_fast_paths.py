"""Fixtures that observe frames and deaths see through the fast paths.

`_testinternalcapi.set_eval_frame_record` logs every frame evaluation, so
frameless leaf calls and generator fast steps have to take real frames while
it's installed, even in code that warmed up without it. A dict or function
watcher sees the death of what `del` releases, even if the instruction is the
core loop's to skip, and a dict watcher sees every store to a watched instance
`__dict__`, even once the store's site has specialized.
"""

import _testcapi
import _testinternalcapi
import unittest

from test.support import catch_unraisable_exception

WARMUP = 100


def record(func):
    calls = []
    try:
        for i in range(WARMUP * 2):
            if i == WARMUP:
                _testinternalcapi.set_eval_frame_record(calls)
            func()
    finally:
        _testinternalcapi.set_eval_frame_default()
    return calls


class EvalFrameProbeTests(unittest.TestCase):
    def assert_records(self, func, names):
        self.assertEqual(record(func), names * WARMUP)

    def test_leaf_calls(self):
        def inner(x=42):
            pass

        def func():
            inner()
            inner(42)

        self.assert_records(func, ["func", "inner", "inner"])

    def test_leaf_property(self):
        class C:
            @property
            def a(self):
                return 42

        class D:
            def __getattribute__(self, name):
                return 42

        def func():
            C().a
            D().a

        self.assert_records(func, ["func", "a", "__getattribute__"])

    def test_yield_from(self):
        def inner():
            yield 42

        def outer():
            yield from inner()

        def func():
            list(outer())

        self.assert_records(
            func, ["func", "outer", "outer", "inner", "inner", "outer", "inner"]
        )

    def test_recording_ends(self):
        def inner():
            pass

        def func():
            inner()

        record(func)
        func()
        self.assertEqual(_testinternalcapi.set_eval_frame_default(), None)


class DeathObserverTests(unittest.TestCase):
    def test_del_of_watched_dict(self):
        d = {}
        wid = _testcapi.add_dict_watcher(1)  # the watcher that raises "boom!"
        try:
            _testcapi.watch_dict(wid, d)
            with catch_unraisable_exception() as cm:
                del d
                self.assertEqual(str(cm.unraisable.exc_value), "boom!")
        finally:
            _testcapi.clear_dict_watcher(wid)

    def test_del_of_watched_function(self):
        class MyError(Exception):
            pass

        def watcher(*args):
            raise MyError("testing 123")

        def myfunc():
            pass

        wid = _testcapi.add_func_watcher(watcher)
        try:
            with catch_unraisable_exception() as cm:
                del myfunc
                self.assertIsInstance(cm.unraisable.exc_value, MyError)
        finally:
            _testcapi.clear_func_watcher(wid)

    def test_warm_store_to_watched_instance_dict(self):
        class C:
            pass

        o = C()
        wid = _testcapi.add_dict_watcher(0)  # the watcher that logs events
        try:
            _testcapi.watch_dict(wid, o.__dict__)
            for _ in range(100):
                o.foo = "bar"
            self.assertEqual(
                _testcapi.get_dict_watcher_events(),
                ["new:foo:bar"] + ["mod:foo:bar"] * 99,
            )
        finally:
            _testcapi.clear_dict_watcher(wid)


if __name__ == "__main__":
    unittest.main()
