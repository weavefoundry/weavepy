"""`hasattr`/`getattr` probes, builtin method calls and lock methods.

`hasattr` answers a method on a built-in kind or a class without
binding it, and must still propagate anything but `AttributeError`.
Builtin method calls release their receiver promptly, and a lock's
context-manager methods find their per-instance implementation by a
remembered position, which must survive other entries changing.
"""

import io
import threading
import unittest


class Finalized:
    count = 0

    def __del__(self):
        Finalized.count += 1


class Props:
    def method(self):
        return 1

    @property
    def missing(self):
        raise AttributeError("missing")

    @property
    def broken(self):
        raise ValueError("broken")


class Dynamic:
    def __getattr__(self, name):
        if name == "boom":
            raise KeyError(name)
        if name.startswith("dyn_"):
            return name
        raise AttributeError(name)


class Failing(Exception):
    def __init__(self, *args):
        super().__init__(*args)


class AttrProbeTest(unittest.TestCase):
    def test_stream_methods(self):
        for s in (io.StringIO(), io.BytesIO()):
            for _ in range(500):
                self.assertTrue(hasattr(s, "write"))
                self.assertTrue(hasattr(s, "getvalue"))
                self.assertFalse(hasattr(s, "no_such_method"))
            s.close()
            self.assertTrue(hasattr(s, "write"))
            self.assertTrue(getattr(s, "closed"))

    def test_builtin_kinds(self):
        for _ in range(500):
            self.assertTrue(hasattr("x", "upper"))
            self.assertTrue(hasattr([], "append"))
            self.assertTrue(hasattr({}, "get"))
            self.assertFalse(hasattr("x", "append"))
            self.assertTrue(hasattr(1, "real"))
            self.assertTrue(hasattr(1.5, "is_integer"))
            self.assertFalse(hasattr((), "_private"))
        self.assertEqual(getattr("abc", "upper")(), "ABC")
        self.assertIsNone(getattr("abc", "nope", None))

    def test_instances(self):
        p = Props()
        for _ in range(500):
            self.assertTrue(hasattr(p, "method"))
            self.assertFalse(hasattr(p, "missing"))
            self.assertFalse(hasattr(p, "nothing"))
            self.assertEqual(getattr(p, "method")(), 1)
            self.assertEqual(getattr(p, "nothing", 7), 7)
        with self.assertRaises(ValueError):
            hasattr(p, "broken")
        p.method = 5
        self.assertTrue(hasattr(p, "method"))
        self.assertEqual(getattr(p, "method"), 5)
        del p.method
        self.assertEqual(getattr(p, "method")(), 1)

    def test_getattr_hook(self):
        d = Dynamic()
        for _ in range(300):
            self.assertTrue(hasattr(d, "dyn_x"))
            self.assertFalse(hasattr(d, "other"))
        with self.assertRaises(KeyError):
            hasattr(d, "boom")
        with self.assertRaises(TypeError):
            hasattr(d, 1)

    def test_modules_and_functions(self):
        import os

        def f():
            pass

        f.tag = "t"
        for _ in range(300):
            self.assertTrue(hasattr(os, "getpid"))
            self.assertFalse(hasattr(os, "no_such_name_here"))
            self.assertTrue(hasattr(f, "tag"))
            self.assertEqual(getattr(f, "tag", None), "t")
            self.assertIsNone(getattr(f, "untagged", None))


class BuiltinLaneTest(unittest.TestCase):
    def test_receiver_released_promptly(self):
        Finalized.count = 0
        for i in range(300):
            Finalized().__reduce_ex__(4)
            self.assertEqual(Finalized.count, i + 1)

    def test_exception_init_through_super(self):
        for i in range(300):
            e = Failing(i, "x")
            self.assertEqual(e.args, (i, "x"))

    def test_bound_builtin_methods(self):
        items = []
        append = items.append
        for i in range(300):
            append(i)
            self.assertEqual("-".join(["a", str(i)]), "a-%d" % i)
        self.assertEqual(len(items), 300)


class LockMethodTest(unittest.TestCase):
    def check_lock(self, lock):
        for _ in range(500):
            with lock:
                self.assertTrue(lock.locked() if hasattr(lock, "locked") else True)
        self.assertFalse(lock.locked())
        try:
            with lock:
                raise RuntimeError("inside")
        except RuntimeError:
            pass
        self.assertFalse(lock.locked())
        self.assertTrue(lock.acquire(False))
        lock.release()

    def test_lock(self):
        self.check_lock(threading.Lock())

    def test_rlock(self):
        r = threading.RLock()
        self.check_lock(r)
        with r:
            with r:
                self.assertTrue(r._is_owned())
        self.assertFalse(r._is_owned())

    def test_many_locks(self):
        locks = [threading.Lock() for _ in range(50)] + [threading.RLock() for _ in range(50)]
        for _ in range(20):
            for lock in locks:
                with lock:
                    pass
                self.assertFalse(lock.locked())

    def test_condition_and_threads(self):
        cond = threading.Condition()
        box = []

        def worker():
            with cond:
                box.append(1)
                cond.notify()

        with cond:
            t = threading.Thread(target=worker)
            t.start()
            while not box:
                cond.wait(5)
        t.join()
        self.assertEqual(box, [1])


if __name__ == "__main__":
    unittest.main()
