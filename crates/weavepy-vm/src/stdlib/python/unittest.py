"""WeavePy `unittest` — pytest-style assertions over Python's classic
TestCase model.

Implements enough of CPython's `unittest` module that test suites
written against `unittest.TestCase` Just Work. Discovery is module-
local: `TestLoader().loadTestsFromModule(mod)` collects test methods
starting with ``test``. The runner streams ``.`` for each pass and
``F``/``E`` for failures, then prints a CPython-shaped summary.
"""

import sys
import time as _time
import traceback as _traceback


__all__ = [
    "TestCase",
    "TestSuite",
    "TestLoader",
    "TextTestRunner",
    "TextTestResult",
    "TestResult",
    "main",
    "expectedFailure",
    "skip",
    "skipIf",
    "skipUnless",
    "SkipTest",
    "FunctionTestCase",
    "defaultTestLoader",
]


class _Outcome:
    def __init__(self):
        self.success = True
        self.skipped = []
        self.expectedFailures = []
        self.unexpectedSuccesses = []
        self.errors = []
        self.failures = []
        self.errors_setup = []


class SkipTest(Exception):
    """Raised inside a test to skip it."""


class _ShouldStop(Exception):
    pass


def _id(obj):
    return obj


def skip(reason):
    def deco(test_item):
        if isinstance(test_item, type):
            test_item.__unittest_skip__ = True
            test_item.__unittest_skip_why__ = reason
            return test_item

        def wrapper(*args, **kwargs):
            raise SkipTest(reason)

        wrapper.__unittest_skip__ = True
        wrapper.__unittest_skip_why__ = reason
        return wrapper

    return deco


def skipIf(condition, reason):
    if condition:
        return skip(reason)
    return _id


def skipUnless(condition, reason):
    if not condition:
        return skip(reason)
    return _id


def expectedFailure(func):
    func.__unittest_expecting_failure__ = True
    return func


class TestResult:
    def __init__(self, stream=None, descriptions=None, verbosity=None):
        self.failures = []
        self.errors = []
        self.testsRun = 0
        self.skipped = []
        self.expectedFailures = []
        self.unexpectedSuccesses = []
        self.shouldStop = False
        self.buffer = False
        self._mirrorOutput = False
        self.stream = stream or sys.stderr
        self._verbosity = verbosity or 1

    def startTest(self, test):
        self.testsRun += 1

    def stopTest(self, test):
        pass

    def startTestRun(self):
        pass

    def stopTestRun(self):
        pass

    def addError(self, test, err):
        self.errors.append((test, self._exc_info_to_string(err, test)))

    def addFailure(self, test, err):
        self.failures.append((test, self._exc_info_to_string(err, test)))

    def addSuccess(self, test):
        pass

    def addSkip(self, test, reason):
        self.skipped.append((test, reason))

    def addExpectedFailure(self, test, err):
        self.expectedFailures.append((test, self._exc_info_to_string(err, test)))

    def addUnexpectedSuccess(self, test):
        self.unexpectedSuccesses.append(test)

    def stop(self):
        self.shouldStop = True

    def wasSuccessful(self):
        return len(self.failures) == 0 and len(self.errors) == 0 and len(self.unexpectedSuccesses) == 0

    def _exc_info_to_string(self, err, test):
        try:
            return "".join(_traceback.format_exception(err[0], err[1], err[2]))
        except Exception:
            return f"{err[0]}: {err[1]}"


class _AssertRaisesContext:
    def __init__(self, expected, regex=None):
        self.expected = expected
        self.regex = regex
        self.exception = None

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, tb):
        if exc_type is None:
            raise AssertionError(f"{self.expected.__name__} not raised")
        if not issubclass(exc_type, self.expected):
            return False
        self.exception = exc_value
        if self.regex is not None:
            import re
            text = str(exc_value)
            if not re.search(self.regex, text):
                raise AssertionError(f"{self.regex!r} does not match {text!r}")
        return True


class _AssertWarnsContext:
    def __init__(self, expected):
        self.expected = expected
        self.warning = None

    def __enter__(self):
        import warnings
        self._cm = warnings.catch_warnings(record=True)
        self._records = self._cm.__enter__()
        warnings.simplefilter("always")
        return self

    def __exit__(self, exc_type, exc_value, tb):
        import warnings
        self._cm.__exit__(exc_type, exc_value, tb)
        if exc_type is not None:
            return False
        for w in self._records:
            if issubclass(w.category, self.expected):
                self.warning = w.message
                return True
        raise AssertionError(f"{self.expected.__name__} not triggered")


class TestCase:
    """Base class for unittests."""

    failureException = AssertionError
    longMessage = True
    maxDiff = 640

    def __init__(self, methodName="runTest"):
        self._testMethodName = methodName
        self._cleanups = []
        method = getattr(self, methodName, None)
        if method is None:
            raise ValueError(f"no such test method {type(self).__name__}.{methodName}")
        self._testMethodDoc = (method.__doc__ or "")

    # -- lifecycle -------------------------------------------------- #

    def setUp(self):
        pass

    def tearDown(self):
        pass

    @classmethod
    def setUpClass(cls):
        pass

    @classmethod
    def tearDownClass(cls):
        pass

    def shortDescription(self):
        doc = self._testMethodDoc
        return doc.strip().split("\n")[0].strip() if doc else None

    def id(self):
        return f"{type(self).__module__}.{type(self).__name__}.{self._testMethodName}"

    def __str__(self):
        return f"{self._testMethodName} ({type(self).__name__})"

    def __repr__(self):
        return f"<{type(self).__name__} testMethod={self._testMethodName}>"

    # -- running ---------------------------------------------------- #

    def addCleanup(self, function, *args, **kwargs):
        self._cleanups.append((function, args, kwargs))

    def doCleanups(self):
        outcome = True
        while self._cleanups:
            fn, args, kwargs = self._cleanups.pop()
            try:
                fn(*args, **kwargs)
            except Exception:
                outcome = False
        return outcome

    def run(self, result=None):
        if result is None:
            result = TestResult()
        result.startTest(self)
        try:
            method = getattr(self, self._testMethodName)
            if getattr(self, "__unittest_skip__", False) or getattr(method, "__unittest_skip__", False):
                reason = getattr(method, "__unittest_skip_why__", None) \
                    or getattr(self, "__unittest_skip_why__", "skipped")
                result.addSkip(self, reason)
                return result
            expecting_failure = getattr(method, "__unittest_expecting_failure__", False)
            try:
                self.setUp()
            except SkipTest as e:
                result.addSkip(self, str(e))
                return result
            except Exception:
                result.addError(self, sys.exc_info())
                return result
            try:
                method()
                ok = True
            except SkipTest as e:
                result.addSkip(self, str(e))
                ok = True
            except self.failureException:
                if expecting_failure:
                    result.addExpectedFailure(self, sys.exc_info())
                else:
                    result.addFailure(self, sys.exc_info())
                ok = False
            except Exception:
                result.addError(self, sys.exc_info())
                ok = False
            else:
                if expecting_failure:
                    result.addUnexpectedSuccess(self)
                else:
                    result.addSuccess(self)
            try:
                self.tearDown()
            except Exception:
                result.addError(self, sys.exc_info())
                ok = False
            self.doCleanups()
        finally:
            result.stopTest(self)
        return result

    def __call__(self, *args, **kwds):
        return self.run(*args, **kwds)

    # -- assertions ------------------------------------------------ #

    def _formatMessage(self, msg, standardMsg):
        if not self.longMessage:
            return msg or standardMsg
        if msg is None:
            return standardMsg
        return standardMsg + " : " + str(msg)

    def fail(self, msg=None):
        raise self.failureException(msg or "")

    def assertEqual(self, first, second, msg=None):
        if not first == second:
            raise self.failureException(self._formatMessage(msg, f"{first!r} != {second!r}"))

    def assertNotEqual(self, first, second, msg=None):
        if first == second:
            raise self.failureException(self._formatMessage(msg, f"{first!r} == {second!r}"))

    def assertTrue(self, expr, msg=None):
        if not expr:
            raise self.failureException(self._formatMessage(msg, f"{expr!r} is not true"))

    def assertFalse(self, expr, msg=None):
        if expr:
            raise self.failureException(self._formatMessage(msg, f"{expr!r} is not false"))

    def assertIs(self, first, second, msg=None):
        if first is not second:
            raise self.failureException(self._formatMessage(msg, f"{first!r} is not {second!r}"))

    def assertIsNot(self, first, second, msg=None):
        if first is second:
            raise self.failureException(self._formatMessage(msg, f"unexpectedly identical: {first!r}"))

    def assertIsNone(self, obj, msg=None):
        if obj is not None:
            raise self.failureException(self._formatMessage(msg, f"{obj!r} is not None"))

    def assertIsNotNone(self, obj, msg=None):
        if obj is None:
            raise self.failureException(self._formatMessage(msg, "unexpectedly None"))

    def assertIn(self, member, container, msg=None):
        if member not in container:
            raise self.failureException(self._formatMessage(msg, f"{member!r} not found in {container!r}"))

    def assertNotIn(self, member, container, msg=None):
        if member in container:
            raise self.failureException(self._formatMessage(msg, f"{member!r} unexpectedly found in {container!r}"))

    def assertIsInstance(self, obj, cls, msg=None):
        if not isinstance(obj, cls):
            raise self.failureException(self._formatMessage(msg, f"{obj!r} is not an instance of {cls!r}"))

    def assertNotIsInstance(self, obj, cls, msg=None):
        if isinstance(obj, cls):
            raise self.failureException(self._formatMessage(msg, f"{obj!r} is an instance of {cls!r}"))

    def assertAlmostEqual(self, first, second, places=7, msg=None, delta=None):
        if first == second:
            return
        if delta is not None and places is not None and places != 7:
            raise TypeError("specify delta or places not both")
        if delta is not None:
            if abs(first - second) <= delta:
                return
            raise self.failureException(self._formatMessage(msg, f"{first!r} != {second!r} within {delta!r} delta"))
        diff = round(abs(first - second), places)
        if diff == 0:
            return
        raise self.failureException(self._formatMessage(msg, f"{first!r} != {second!r} within {places} places"))

    def assertNotAlmostEqual(self, first, second, places=7, msg=None, delta=None):
        try:
            self.assertAlmostEqual(first, second, places=places, delta=delta)
        except self.failureException:
            return
        raise self.failureException(self._formatMessage(msg, f"{first!r} == {second!r} (almost equal)"))

    def assertGreater(self, a, b, msg=None):
        if not a > b:
            raise self.failureException(self._formatMessage(msg, f"{a!r} not greater than {b!r}"))

    def assertGreaterEqual(self, a, b, msg=None):
        if not a >= b:
            raise self.failureException(self._formatMessage(msg, f"{a!r} not greater than or equal to {b!r}"))

    def assertLess(self, a, b, msg=None):
        if not a < b:
            raise self.failureException(self._formatMessage(msg, f"{a!r} not less than {b!r}"))

    def assertLessEqual(self, a, b, msg=None):
        if not a <= b:
            raise self.failureException(self._formatMessage(msg, f"{a!r} not less than or equal to {b!r}"))

    def assertCountEqual(self, first, second, msg=None):
        a = list(first)
        b = list(second)
        if sorted(a, key=repr) != sorted(b, key=repr):
            raise self.failureException(self._formatMessage(msg, f"{first!r} != {second!r}"))

    def assertSequenceEqual(self, seq1, seq2, msg=None, seq_type=None):
        if seq_type is not None:
            if not isinstance(seq1, seq_type) or not isinstance(seq2, seq_type):
                raise self.failureException(self._formatMessage(msg, f"unexpected sequence type"))
        if list(seq1) != list(seq2):
            raise self.failureException(self._formatMessage(msg, f"{seq1!r} != {seq2!r}"))

    def assertListEqual(self, l1, l2, msg=None):
        self.assertSequenceEqual(l1, l2, msg=msg, seq_type=list)

    def assertTupleEqual(self, t1, t2, msg=None):
        self.assertSequenceEqual(t1, t2, msg=msg, seq_type=tuple)

    def assertDictEqual(self, d1, d2, msg=None):
        if d1 != d2:
            raise self.failureException(self._formatMessage(msg, f"{d1!r} != {d2!r}"))

    def assertSetEqual(self, s1, s2, msg=None):
        if set(s1) != set(s2):
            raise self.failureException(self._formatMessage(msg, f"{s1!r} != {s2!r}"))

    def assertRegex(self, text, regex, msg=None):
        import re
        if not re.search(regex, text):
            raise self.failureException(self._formatMessage(msg, f"{regex!r} not found in {text!r}"))

    def assertNotRegex(self, text, regex, msg=None):
        import re
        if re.search(regex, text):
            raise self.failureException(self._formatMessage(msg, f"{regex!r} unexpectedly found in {text!r}"))

    def assertRaises(self, expected, callable_=None, *args, **kwargs):
        if callable_ is None:
            return _AssertRaisesContext(expected)
        try:
            callable_(*args, **kwargs)
        except expected:
            return
        raise self.failureException(f"{expected.__name__} not raised")

    def assertRaisesRegex(self, expected, regex, callable_=None, *args, **kwargs):
        if callable_ is None:
            return _AssertRaisesContext(expected, regex)
        ctx = _AssertRaisesContext(expected, regex)
        with ctx:
            callable_(*args, **kwargs)

    def assertWarns(self, expected, callable_=None, *args, **kwargs):
        if callable_ is None:
            return _AssertWarnsContext(expected)
        ctx = _AssertWarnsContext(expected)
        with ctx:
            callable_(*args, **kwargs)

    failUnless = assertTrue
    failIf = assertFalse
    failUnlessEqual = assertEqual
    failIfEqual = assertNotEqual


class FunctionTestCase(TestCase):
    """Wrap a single function as a TestCase."""

    def __init__(self, func, setUp=None, tearDown=None, description=None):
        super().__init__("runTest")
        self._setUpFunc = setUp
        self._tearDownFunc = tearDown
        self._testFunc = func
        self._description = description

    def setUp(self):
        if self._setUpFunc is not None:
            self._setUpFunc()

    def tearDown(self):
        if self._tearDownFunc is not None:
            self._tearDownFunc()

    def runTest(self):
        self._testFunc()

    def id(self):
        return self._testFunc.__name__

    def shortDescription(self):
        return self._description or self._testFunc.__doc__

    def __str__(self):
        return f"{self._testFunc.__name__} ({type(self).__name__})"


class TestSuite:
    def __init__(self, tests=()):
        self._tests = []
        self.addTests(tests)

    def addTest(self, test):
        if not callable(test) and not isinstance(test, TestSuite):
            raise TypeError("test is not callable")
        self._tests.append(test)

    def addTests(self, tests):
        for t in tests:
            if isinstance(t, TestSuite):
                self.addTest(t)
            else:
                self.addTest(t)

    def countTestCases(self):
        return sum(1 for _ in self)

    def run(self, result, debug=False):
        for test in self:
            if result.shouldStop:
                break
            test.run(result)
        return result

    def __iter__(self):
        for t in self._tests:
            if isinstance(t, TestSuite):
                yield from t
            else:
                yield t

    def __call__(self, result):
        return self.run(result)


class TestLoader:
    testMethodPrefix = "test"
    sortTestMethodsUsing = staticmethod(lambda a, b: (a > b) - (a < b))
    suiteClass = TestSuite

    def loadTestsFromTestCase(self, testCaseClass):
        names = self.getTestCaseNames(testCaseClass)
        suite = self.suiteClass([testCaseClass(name) for name in names])
        return suite

    def loadTestsFromModule(self, module):
        suite = self.suiteClass()
        for name in dir(module):
            obj = getattr(module, name)
            if isinstance(obj, type) and issubclass(obj, TestCase) and obj is not TestCase:
                suite.addTest(self.loadTestsFromTestCase(obj))
        return suite

    def getTestCaseNames(self, testCaseClass):
        def is_test(name):
            if not name.startswith(self.testMethodPrefix):
                return False
            attr = getattr(testCaseClass, name, None)
            return callable(attr)

        names = [n for n in dir(testCaseClass) if is_test(n)]
        names.sort()
        return names


defaultTestLoader = TestLoader()


class TextTestResult(TestResult):
    separator1 = "=" * 70
    separator2 = "-" * 70

    def __init__(self, stream=None, descriptions=True, verbosity=1):
        super().__init__(stream=stream, verbosity=verbosity)
        self.descriptions = descriptions
        self.verbosity = verbosity

    def startTest(self, test):
        super().startTest(test)
        if self.verbosity > 1:
            self.stream.write(str(test) + " ... ")

    def addSuccess(self, test):
        super().addSuccess(test)
        if self.verbosity > 1:
            self.stream.write("ok\n")
        elif self.verbosity:
            self.stream.write(".")

    def addError(self, test, err):
        super().addError(test, err)
        if self.verbosity > 1:
            self.stream.write("ERROR\n")
        elif self.verbosity:
            self.stream.write("E")

    def addFailure(self, test, err):
        super().addFailure(test, err)
        if self.verbosity > 1:
            self.stream.write("FAIL\n")
        elif self.verbosity:
            self.stream.write("F")

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        if self.verbosity > 1:
            self.stream.write(f"skipped ({reason!r})\n")
        elif self.verbosity:
            self.stream.write("s")

    def printErrors(self):
        if self.verbosity and (self.errors or self.failures):
            self.stream.write("\n")
        self.printErrorList("ERROR", self.errors)
        self.printErrorList("FAIL", self.failures)

    def printErrorList(self, flavour, errors):
        for test, err in errors:
            self.stream.write(self.separator1 + "\n")
            self.stream.write(f"{flavour}: {test}\n")
            self.stream.write(self.separator2 + "\n")
            self.stream.write(err + "\n")


class TextTestRunner:
    resultclass = TextTestResult

    def __init__(self, stream=None, descriptions=True, verbosity=1, *,
                 failfast=False, buffer=False, resultclass=None, warnings=None):
        self.stream = stream or sys.stderr
        self.descriptions = descriptions
        self.verbosity = verbosity
        self.failfast = failfast
        self.buffer = buffer
        if resultclass is not None:
            self.resultclass = resultclass

    def _makeResult(self):
        return self.resultclass(self.stream, self.descriptions, self.verbosity)

    def run(self, test):
        result = self._makeResult()
        start = _time.time()
        try:
            test(result)
        finally:
            stop = _time.time()
        elapsed = stop - start
        if self.verbosity:
            self.stream.write("\n")
        result.printErrors()
        run = result.testsRun
        self.stream.write(self.resultclass.separator2 + "\n")
        self.stream.write(f"Ran {run} test{'s' if run != 1 else ''} in {elapsed:.3f}s\n\n")
        ok = result.wasSuccessful()
        infos = []
        if not ok:
            self.stream.write("FAILED")
            if result.failures:
                infos.append(f"failures={len(result.failures)}")
            if result.errors:
                infos.append(f"errors={len(result.errors)}")
        else:
            self.stream.write("OK")
        if result.skipped:
            infos.append(f"skipped={len(result.skipped)}")
        if result.expectedFailures:
            infos.append(f"expected failures={len(result.expectedFailures)}")
        if result.unexpectedSuccesses:
            infos.append(f"unexpected successes={len(result.unexpectedSuccesses)}")
        if infos:
            self.stream.write(" (" + ", ".join(infos) + ")")
        self.stream.write("\n")
        return result


def main(module="__main__", defaultTest=None, argv=None, testRunner=None,
         testLoader=defaultTestLoader, exit=True, verbosity=1, failfast=None,
         catchbreak=None, buffer=None, warnings=None):
    if isinstance(module, str):
        module = sys.modules.get(module)
    if testRunner is None:
        testRunner = TextTestRunner(verbosity=verbosity)
    elif isinstance(testRunner, type):
        testRunner = testRunner()
    suite = testLoader.loadTestsFromModule(module)
    result = testRunner.run(suite)
    if exit:
        sys.exit(0 if result.wasSuccessful() else 1)
    return result
