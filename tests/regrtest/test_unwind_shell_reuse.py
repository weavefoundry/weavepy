"""Activations an exception unwinds through keep their frames for the
traceback, and those frames' storage is reused once the traceback dies;
the frames a kept traceback shows, the values its frames release and
when, and the locals of later calls match a fresh frame each time."""

import gc
import sys
import unittest
import weakref


class E(Exception):
    pass


class Tracked:
    deleted = 0

    def __del__(self):
        Tracked.deleted += 1


def thrower(n):
    if n:
        raise E(n)
    return 0


def deep(n):
    return thrower(n) if n < 2 else deep(n - 1)


def holder(n, obj):
    local = obj
    if n:
        return holder(n - 1, obj)
    raise E(local)


def closure_raiser(n):
    cell = [n]

    def inner():
        raise E(cell[0])

    if n:
        return closure_raiser(n - 1)
    return inner()


def fresh_locals(a, b=None):
    # A reused locals vector must start with only the arguments.
    c = a + 1
    if b is None:
        return c, "b unbound" if "d" not in locals() else "leak"
    d = b
    return c, d


def gen_raiser():
    yield 1
    raise E("gen")


class UnwindShellReuse(unittest.TestCase):
    def test_repeated_deep_raise(self):
        for i in range(3000):
            try:
                deep(5)
            except E as e:
                self.assertEqual(e.args, (1,))
            self.assertEqual(fresh_locals(i), (i + 1, "b unbound"))
            self.assertEqual(fresh_locals(i, 2), (i + 1, 2))

    def test_kept_traceback_frames(self):
        for _ in range(500):
            try:
                deep(5)
            except E:
                pass
        kept = []
        for _ in range(50):
            try:
                deep(4)
            except E as e:
                kept.append(e.__traceback__)
            for _ in range(20):
                try:
                    deep(5)
                except E:
                    pass
        for tb in kept:
            names = []
            ns = []
            while tb is not None:
                names.append(tb.tb_frame.f_code.co_name)
                if tb.tb_frame.f_code.co_name != "test_kept_traceback_frames":
                    ns.append(tb.tb_frame.f_locals["n"])
                tb = tb.tb_next
            self.assertEqual(
                names,
                ["test_kept_traceback_frames", "deep", "deep", "deep", "deep", "thrower"],
            )
            self.assertEqual(ns, [4, 3, 2, 1, 1])

    def test_values_released_with_traceback(self):
        for _ in range(300):
            try:
                deep(3)
            except E:
                pass
        for _ in range(200):
            before = Tracked.deleted
            obj = Tracked()
            ref = weakref.ref(obj)
            try:
                holder(3, obj)
            except E as e:
                caught = e
            del obj
            self.assertIsNotNone(ref())
            del caught
            self.assertIsNone(ref())
            self.assertEqual(Tracked.deleted, before + 1)

    def test_traceback_kept_then_read_after_reuse(self):
        try:
            holder(2, "kept")
        except E as e:
            tb = e.__traceback__
        for _ in range(500):
            try:
                holder(3, "other")
            except E:
                pass
        values = []
        while tb is not None:
            f = tb.tb_frame
            if f.f_code.co_name == "holder":
                values.append((f.f_locals["n"], f.f_locals["local"]))
            tb = tb.tb_next
        self.assertEqual(values, [(2, "kept"), (1, "kept"), (0, "kept")])

    def test_closure_frames(self):
        for i in range(1000):
            try:
                closure_raiser(3)
            except E as e:
                self.assertEqual(e.args, (0,))
                if i % 100 == 0:
                    tb = e.__traceback__
                    depth = 0
                    while tb is not None:
                        depth += 1
                        tb = tb.tb_next
                    self.assertEqual(depth, 6)

    def test_generator_in_chain(self):
        for _ in range(500):
            g = gen_raiser()
            self.assertEqual(next(g), 1)
            try:
                next(g)
            except E as e:
                self.assertEqual(e.args, ("gen",))
            try:
                deep(3)
            except E:
                pass

    def test_f_back_of_departed_frame(self):
        for _ in range(300):
            try:
                deep(3)
            except E as e:
                tb = e.__traceback__
                while tb.tb_next is not None:
                    tb = tb.tb_next
                frame = tb.tb_frame
                self.assertEqual(frame.f_code.co_name, "thrower")
                self.assertEqual(frame.f_back.f_code.co_name, "deep")
                del frame, tb

    def test_collect_between(self):
        for i in range(200):
            try:
                deep(4)
            except E as e:
                saved = e
            if i % 50 == 0:
                gc.collect()
            del saved
        self.assertTrue(sys.getrefcount(E) > 0)


if __name__ == "__main__":
    unittest.main()
