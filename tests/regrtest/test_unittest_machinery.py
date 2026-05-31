"""Self-host fixture: exercise the ``unittest`` framework itself.

RFC 0034 hardens ``unittest`` so CPython's tests (which are ``unittest``
modules) can run. This fixture drives the assertion vocabulary, the
skip / expectedFailure decorators, ``setUp``/``tearDown`` ordering,
``subTest``, and the loader/suite/result plumbing — and, crucially,
runs a nested suite *programmatically* so we verify result bookkeeping
(counts of passes, failures, errors, skips) rather than just trusting
the outer runner.
"""

import unittest


class AssertionVocabularyTests(unittest.TestCase):
    def test_equality_family(self):
        self.assertEqual(2 + 2, 4)
        self.assertNotEqual(2 + 2, 5)
        self.assertTrue([0])
        self.assertFalse([])
        self.assertIsNone(None)
        self.assertIsNotNone(0)

    def test_identity_and_membership(self):
        obj = object()
        self.assertIs(obj, obj)
        self.assertIsNot(obj, object())
        self.assertIn(3, [1, 2, 3])
        self.assertNotIn(4, [1, 2, 3])

    def test_isinstance(self):
        self.assertIsInstance(1, int)
        self.assertNotIsInstance(1, str)

    def test_ordering(self):
        self.assertGreater(3, 2)
        self.assertGreaterEqual(3, 3)
        self.assertLess(2, 3)
        self.assertLessEqual(3, 3)

    def test_almost_equal(self):
        self.assertAlmostEqual(0.1 + 0.2, 0.3, places=7)
        self.assertNotAlmostEqual(0.1, 0.2, places=7)

    def test_container_equality(self):
        self.assertListEqual([1, 2], [1, 2])
        self.assertTupleEqual((1, 2), (1, 2))
        self.assertDictEqual({"a": 1}, {"a": 1})
        self.assertSetEqual({1, 2}, {2, 1})
        self.assertCountEqual([1, 2, 2], [2, 1, 2])

    def test_regex(self):
        self.assertRegex("hello world", r"wor")
        self.assertNotRegex("hello", r"zzz")

    def test_multiline(self):
        self.assertMultiLineEqual("a\nb", "a\nb")


class RaisesTests(unittest.TestCase):
    def test_assert_raises_context(self):
        with self.assertRaises(ValueError):
            raise ValueError("boom")

    def test_assert_raises_callable(self):
        self.assertRaises(KeyError, lambda: {}["missing"])

    def test_assert_raises_regex(self):
        with self.assertRaisesRegex(RuntimeError, r"down"):
            raise RuntimeError("meltdown")

    def test_exception_captured(self):
        with self.assertRaises(ValueError) as cm:
            raise ValueError("payload")
        self.assertEqual(str(cm.exception), "payload")


class LifecycleTests(unittest.TestCase):
    order = []

    @classmethod
    def setUpClass(cls):
        cls.order.append("setUpClass")

    @classmethod
    def tearDownClass(cls):
        cls.order.append("tearDownClass")

    def setUp(self):
        type(self).order.append("setUp")

    def tearDown(self):
        type(self).order.append("tearDown")

    def test_a(self):
        type(self).order.append("test_a")

    def test_b(self):
        type(self).order.append("test_b")


class SkipTests(unittest.TestCase):
    @unittest.skip("unconditional skip")
    def test_unconditional(self):
        self.fail("should never run")

    @unittest.skipIf(True, "skip when true")
    def test_skip_if(self):
        self.fail("should never run")

    @unittest.skipUnless(False, "skip unless true")
    def test_skip_unless(self):
        self.fail("should never run")

    def test_skiptest_call(self):
        self.skipTest("explicit skipTest")
        self.fail("should never run")

    @unittest.expectedFailure
    def test_expected_failure(self):
        self.assertEqual(1, 2)


class SubTestTests(unittest.TestCase):
    def test_subtests(self):
        for i in range(4):
            with self.subTest(i=i):
                self.assertEqual(i % 2, 0 if i % 2 == 0 else 1)


def _make_sample_case():
    """Build a deliberately-mixed ``TestCase`` *locally*.

    Defined inside a function (not at module scope) so the outer loader
    never discovers it — only the programmatic runs below see its
    intentional failure/error/skip.
    """
    class _Sample(unittest.TestCase):
        def test_pass(self):
            self.assertTrue(True)

        def test_fail(self):
            self.assertEqual(1, 2)

        def test_error(self):
            raise RuntimeError("kaboom")

        @unittest.skip("nope")
        def test_skip(self):
            pass

    return _Sample


class ResultBookkeepingTests(unittest.TestCase):
    def _run_sample(self):
        loader = unittest.TestLoader()
        suite = loader.loadTestsFromTestCase(_make_sample_case())
        result = unittest.TestResult()
        suite.run(result)
        return result

    def test_counts(self):
        result = self._run_sample()
        self.assertEqual(result.testsRun, 4)
        self.assertEqual(len(result.failures), 1)
        self.assertEqual(len(result.errors), 1)
        self.assertEqual(len(result.skipped), 1)
        self.assertFalse(result.wasSuccessful())

    def test_loader_counts_cases(self):
        loader = unittest.TestLoader()
        suite = loader.loadTestsFromTestCase(_make_sample_case())
        self.assertEqual(suite.countTestCases(), 4)

    def test_suite_compose(self):
        loader = unittest.TestLoader()
        suite = unittest.TestSuite()
        suite.addTest(loader.loadTestsFromTestCase(AssertionVocabularyTests))
        self.assertGreater(suite.countTestCases(), 0)


def load_tests(loader, standard_tests, pattern):
    # Exercise the load_tests protocol the runner honours.
    return standard_tests


if __name__ == "__main__":
    unittest.main()
