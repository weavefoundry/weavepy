"""ContextVar.get from compiled loops (an exact var's state read in
place): defaults given to the var and to get, LookupError for a var with
neither, values set and reset, values inside copy_context().run, and
values seen from another thread, each matching CPython."""

import contextvars
import threading
import unittest

A = contextvars.ContextVar("a", default=1)
B = contextvars.ContextVar("b")


def gets(n, var, *default):
    out = None
    for i in range(n):
        out = var.get(*default)
    return out


class ContextVarGetTests(unittest.TestCase):
    def test_defaults_and_errors(self):
        for n in (1, 300):
            self.assertEqual(gets(n, A), 1)
            self.assertEqual(gets(n, B, "dflt"), "dflt")
            with self.assertRaises(LookupError):
                gets(n, B)

    def test_set_reset_and_contexts(self):
        tok = A.set(5)
        try:
            self.assertEqual(gets(300, A), 5)

            def inner():
                A.set(9)
                return gets(300, A)

            self.assertEqual(contextvars.copy_context().run(inner), 9)
            self.assertEqual(gets(300, A), 5)
        finally:
            A.reset(tok)
        self.assertEqual(gets(300, A), 1)

    def test_other_thread(self):
        tok = A.set(7)
        try:
            seen = []
            th = threading.Thread(target=lambda: seen.append(gets(300, A)))
            th.start()
            th.join()
            self.assertEqual(seen, [1])
            self.assertEqual(gets(300, A), 7)
        finally:
            A.reset(tok)


if __name__ == "__main__":
    unittest.main()
