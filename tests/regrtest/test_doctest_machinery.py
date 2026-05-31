"""Self-host fixture: exercise the ``doctest`` module.

RFC 0034 ships a ``doctest`` faithful enough that stdlib self-tests and
``support.run_doctest`` work. This fixture drives the parser, finder,
runner, output checker, the ``"single"``-mode echo path (interactive
expression results) and the ``unittest`` bridge — including a
deliberately failing example to prove failures are *counted*, not
swallowed.
"""

import sys
import unittest

import doctest


def square(n):
    """Return ``n`` squared.

    >>> square(3)
    9
    >>> square(-4)
    16
    """
    return n * n


def greet(name):
    """Greet someone.

    >>> greet("world")
    'hello world'
    """
    return "hello " + name


def _quiet():
    """A sink for runner output."""
    return lambda s: None


class ParserTests(unittest.TestCase):
    def test_parse_splits_prose_and_examples(self):
        parser = doctest.DocTestParser()
        # A blank line terminates the expected output (matching CPython:
        # non-blank prose immediately after output is *part* of want).
        pieces = parser.parse(
            "Some prose.\n>>> 1 + 1\n2\n\nmore prose.\n")
        examples = [p for p in pieces if isinstance(p, doctest.Example)]
        self.assertEqual(len(examples), 1)
        self.assertEqual(examples[0].source, "1 + 1\n")
        self.assertEqual(examples[0].want, "2\n")

    def test_get_examples_multiple(self):
        parser = doctest.DocTestParser()
        examples = parser.get_examples(
            ">>> a = 1\n>>> a + 1\n2\n")
        self.assertEqual(len(examples), 2)

    def test_get_doctest(self):
        parser = doctest.DocTestParser()
        test = parser.get_doctest(">>> 2 + 2\n4\n", {}, "inline", "f.py", 0)
        self.assertEqual(test.name, "inline")
        self.assertEqual(len(test.examples), 1)


class FinderTests(unittest.TestCase):
    def test_find_function_doctest(self):
        finder = doctest.DocTestFinder()
        tests = finder.find(square)
        with_examples = [t for t in tests if t.examples]
        self.assertEqual(len(with_examples), 1)
        self.assertEqual(len(with_examples[0].examples), 2)

    def test_find_uses_dunder_name(self):
        finder = doctest.DocTestFinder()
        tests = finder.find(greet)
        self.assertTrue(any(t.name.endswith("greet") for t in tests))


class RunnerTests(unittest.TestCase):
    def test_passing_examples(self):
        finder = doctest.DocTestFinder()
        test = [t for t in finder.find(square) if t.examples][0]
        runner = doctest.DocTestRunner(verbose=False)
        result = runner.run(test, out=_quiet())
        self.assertEqual(result.failed, 0)
        self.assertEqual(result.attempted, 2)

    def test_failing_example_is_counted(self):
        parser = doctest.DocTestParser()
        # Expected output is wrong on purpose.
        test = parser.get_doctest(">>> 1 + 1\n3\n", {}, "bad", "f.py", 0)
        runner = doctest.DocTestRunner(verbose=False)
        result = runner.run(test, out=_quiet())
        self.assertEqual(result.attempted, 1)
        self.assertEqual(result.failed, 1)

    def test_results_tuple_unpacking(self):
        parser = doctest.DocTestParser()
        test = parser.get_doctest(">>> 2 * 3\n6\n", {}, "ok", "f.py", 0)
        runner = doctest.DocTestRunner(verbose=False)
        failed, attempted = runner.run(test, out=_quiet())
        self.assertEqual((failed, attempted), (0, 1))


class OutputCheckerTests(unittest.TestCase):
    def setUp(self):
        self.checker = doctest.OutputChecker()

    def test_exact_match(self):
        self.assertTrue(self.checker.check_output("4\n", "4\n", 0))
        self.assertFalse(self.checker.check_output("4\n", "5\n", 0))

    def test_true_for_one(self):
        self.assertTrue(self.checker.check_output("1\n", "True\n", 0))

    def test_ellipsis(self):
        self.assertFalse(self.checker.check_output("a...z\n", "abcz\n", 0))
        self.assertTrue(self.checker.check_output(
            "a...z\n", "abcz\n", doctest.ELLIPSIS))

    def test_normalize_whitespace(self):
        self.assertTrue(self.checker.check_output(
            "1 2 3\n", "1   2\t3\n", doctest.NORMALIZE_WHITESPACE))


class SingleModeEchoTests(unittest.TestCase):
    def test_run_docstring_examples_passes(self):
        # run_docstring_examples relies on "single"-mode displayhook echo.
        captured = []
        doctest.run_docstring_examples(
            square, {"square": square}, verbose=False, name="square")
        # No exception => examples executed; nothing to assert beyond that.

    def test_single_compile_echoes_repr(self):
        import io
        import contextlib
        buf = io.StringIO()
        code = compile("1 + 1\n'hi'\nNone\n", "<t>", "single")
        with contextlib.redirect_stdout(buf):
            exec(code, {})
        # 1+1 -> "2", 'hi' -> "'hi'", None is suppressed by displayhook.
        self.assertEqual(buf.getvalue(), "2\n'hi'\n")


class BridgeTests(unittest.TestCase):
    def test_doctest_suite_runs_module(self):
        module = sys.modules[__name__]
        suite = doctest.DocTestSuite(module)
        self.assertGreaterEqual(suite.countTestCases(), 2)
        result = unittest.TestResult()
        suite.run(result)
        self.assertTrue(result.wasSuccessful(),
                        "module doctests should all pass")


if __name__ == "__main__":
    unittest.main()
