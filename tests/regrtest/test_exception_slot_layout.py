"""Exception pseudo-slots laid out from birth, and handled-exception state.

An exception instance's `args`, `__traceback__` and chaining links (and a
family's own fields, such as `StopIteration.value`) live in a slot layout
shared by every instance of the shape. Storing a field the layout doesn't
name, deleting one, or reading one that was never set must behave as
before. The handled-exception stack holds only the instances, so
`sys.exc_info()`, bare `raise`, generator suspension inside handlers and
`gen.throw()` chaining must still see the right exception.
"""

import copy
import io
import pickle
import sys
import unittest


def raiser(exc):
    raise exc


def outer(exc):
    raiser(exc)


class AppError(Exception):
    def __init__(self, code, msg):
        super().__init__(msg)
        self.code = code


class SlottedError(Exception):
    __slots__ = ("extra",)


class SlotLayoutTest(unittest.TestCase):
    def test_defaults_before_raise(self):
        for e in (ValueError("x"), StopIteration(3), KeyError("k"), AppError(1, "m")):
            self.assertIsNone(e.__traceback__)
            self.assertIsNone(e.__context__)
            self.assertIsNone(e.__cause__)
            self.assertFalse(e.__suppress_context__)
        self.assertEqual(StopIteration(3).value, 3)
        self.assertIsNone(StopIteration().value)
        self.assertEqual(StopIteration(1, 2).args, (1, 2))

    def test_traceback_chain_across_frames(self):
        try:
            outer(ValueError("deep"))
        except ValueError as e:
            tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names, ["test_traceback_chain_across_frames", "outer", "raiser"])

    def test_reraise_keeps_and_extends_traceback(self):
        def middle():
            try:
                raiser(KeyError("k"))
            except KeyError:
                raise

        try:
            middle()
        except KeyError as e:
            tb = e.__traceback__
        names = []
        while tb is not None:
            names.append(tb.tb_frame.f_code.co_name)
            tb = tb.tb_next
        self.assertEqual(names, ["test_reraise_keeps_and_extends_traceback", "middle", "raiser"])

    def test_traceback_assignment(self):
        try:
            raiser(ValueError())
        except ValueError as e:
            err = e
        err.__traceback__ = None
        self.assertIsNone(err.__traceback__)
        try:
            raise err
        except ValueError as e:
            self.assertIsNotNone(e.__traceback__)
            self.assertIsNone(e.__traceback__.tb_next)
        with self.assertRaises(TypeError):
            err.__traceback__ = 5
        self.assertIs(err.with_traceback(None), err)
        self.assertIsNone(err.__traceback__)

    def test_chaining(self):
        try:
            try:
                raise KeyError("inner")
            except KeyError:
                raise ValueError("outer")
        except ValueError as e:
            self.assertIsInstance(e.__context__, KeyError)
            self.assertIsNone(e.__cause__)
            self.assertFalse(e.__suppress_context__)
        try:
            try:
                raise KeyError("inner")
            except KeyError as k:
                raise AppError(404, "missing") from None
        except AppError as e:
            self.assertIsNone(e.__cause__)
            self.assertTrue(e.__suppress_context__)
            self.assertIsInstance(e.__context__, KeyError)
            self.assertEqual(e.code, 404)
            self.assertEqual(e.args, ("missing",))
        try:
            raise ValueError("v") from TypeError("t")
        except ValueError as e:
            self.assertIsInstance(e.__cause__, TypeError)
            self.assertTrue(e.__suppress_context__)
        e = ValueError()
        e.__context__ = KeyError()
        e.__cause__ = TypeError()
        e.__suppress_context__ = False
        self.assertIsInstance(e.__context__, KeyError)
        self.assertIsInstance(e.__cause__, TypeError)
        self.assertFalse(e.__suppress_context__)
        e.__context__ = None
        self.assertIsNone(e.__context__)

    def test_family_fields(self):
        try:
            object().missing
        except AttributeError as e:
            self.assertEqual(e.name, "missing")
            self.assertIsInstance(e.obj, object)
            self.assertIsNotNone(e.__traceback__)
        try:
            next(iter(()))
        except StopIteration as e:
            self.assertIsNone(e.value)
            self.assertIsNotNone(e.__traceback__)

        def gen():
            yield 1
            return "done"

        g = gen()
        next(g)
        try:
            next(g)
        except StopIteration as e:
            self.assertEqual(e.value, "done")
            self.assertEqual(e.args, ("done",))
        try:
            raise OSError(2, "No such file", "f.txt")
        except FileNotFoundError as e:
            self.assertEqual((e.errno, e.strerror, e.filename), (2, "No such file", "f.txt"))
            self.assertIsNotNone(e.__traceback__)
        try:
            raise SystemExit(3)
        except SystemExit as e:
            self.assertEqual(e.code, 3)
            self.assertIsNotNone(e.__traceback__)
        e = ImportError("no module", name="m", path="/p")
        self.assertEqual((e.msg, e.name, e.path), ("no module", "m", "/p"))

    def test_python_init(self):
        class NoSuper(Exception):
            def __init__(self, a, b):
                self.total = a + b

        e = NoSuper(1, 2)
        self.assertEqual(e.args, (1, 2))
        self.assertEqual(e.total, 3)
        e = AppError(404, "missing")
        self.assertEqual(e.args, ("missing",))
        try:
            try:
                {}["k"]
            except KeyError:
                raise AppError(5, "five") from None
        except AppError as err:
            self.assertEqual((err.code, err.args), (5, ("five",)))
            self.assertTrue(err.__suppress_context__)
            self.assertIsInstance(err.__context__, KeyError)
            self.assertIsNotNone(err.__traceback__)

    def test_cycle_through_args(self):
        import gc
        import weakref

        class Holder:
            pass

        class Err(Exception):
            def __init__(self, holder):
                super().__init__(holder)

        h = Holder()
        h.err = Err(h)
        ref = weakref.ref(h)
        del h
        gc.collect()
        self.assertIsNone(ref())
        self.assertTrue(gc.is_tracked(Err(Holder())))

    def test_slotted_subclass(self):
        try:
            e = SlottedError("x")
            e.extra = 5
            raise e
        except SlottedError as err:
            self.assertEqual(err.extra, 5)
            self.assertEqual(err.args, ("x",))
            self.assertIsNotNone(err.__traceback__)
        e = SlottedError()
        with self.assertRaises(AttributeError):
            e.extra

    def test_copy_and_pickle(self):
        try:
            err = KeyError("seven")
            err.code = 7
            raise err
        except KeyError as e:
            err = e
        for clone in (copy.copy(err), pickle.loads(pickle.dumps(err))):
            self.assertEqual(clone.args, ("seven",))
            self.assertEqual(clone.code, 7)
            self.assertIsNone(clone.__traceback__)
        si = pickle.loads(pickle.dumps(StopIteration(4)))
        self.assertEqual((si.args, si.value), ((4,), 4))


class HandledStateTest(unittest.TestCase):
    def test_exc_info_nesting(self):
        self.assertEqual(sys.exc_info(), (None, None, None))
        try:
            raise KeyError("a")
        except KeyError as a:
            self.assertIs(sys.exc_info()[1], a)
            self.assertIs(sys.exception(), a)
            try:
                raise ValueError("b")
            except ValueError as b:
                self.assertIs(sys.exc_info()[1], b)
                self.assertIs(b.__context__, a)
                self.assertIs(sys.exc_info()[2], b.__traceback__)
            self.assertIs(sys.exc_info()[1], a)
        self.assertIsNone(sys.exception())

    def test_bare_raise_from_helper(self):
        def reraise():
            raise

        try:
            try:
                raise KeyError("k")
            except KeyError as k:
                original = k
                reraise()
        except KeyError as e:
            self.assertIs(e, original)

    def test_generator_suspended_in_handler(self):
        def gen():
            try:
                raise KeyError("in gen")
            except KeyError:
                yield sys.exception()
                yield sys.exception()

        g = gen()
        first = next(g)
        self.assertIsInstance(first, KeyError)
        self.assertIsNone(sys.exception())
        try:
            raise ValueError("outside")
        except ValueError as outside:
            self.assertIs(next(g), first)
            self.assertIs(sys.exception(), outside)
        g.close()

    def test_throw_chains_suspended_handler(self):
        def gen():
            try:
                raise KeyError("handled")
            except KeyError:
                yield 1

        g = gen()
        next(g)
        with self.assertRaises(ValueError) as cm:
            g.throw(ValueError("thrown"))
        self.assertIsInstance(cm.exception.__context__, KeyError)

    def test_unraisable_traceback_order(self):
        def inner():
            raise RuntimeError("from del")

        class Noisy:
            def __del__(self):
                inner()

        saved = sys.stderr
        sys.stderr = buf = io.StringIO()
        try:
            Noisy()
        finally:
            sys.stderr = saved
        text = buf.getvalue()
        self.assertIn("RuntimeError: from del", text)
        self.assertLess(text.index("__del__"), text.index("in inner"))


if __name__ == "__main__":
    unittest.main()
