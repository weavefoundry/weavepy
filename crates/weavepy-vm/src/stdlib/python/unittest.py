"""WeavePy ``unittest`` — a faithful subset of CPython's xUnit framework.

RFC 0034 hardens the RFC 0018 first cut into something real
``Lib/test/`` modules run against: ``TestCase`` with ``subTest`` and the
full ``assert*`` family, once-per-class / once-per-module fixtures, a
``TestLoader`` with ``loadTestsFromName[s]`` and ``discover``, and an
argv-parsing ``TestProgram`` (``main``) so ``python -m unittest`` and
``python -m unittest discover`` behave.

The module is registered as a package whose ``__init__`` is this file;
``unittest.mock`` / ``unittest.async_case`` / ``unittest.__main__`` are
sibling frozen submodules.
"""

import sys
import time as _time
import traceback as _traceback
import contextlib as _contextlib
# Exposed as a module attribute so the flattened `unittest.case` alias
# (built at the bottom of this module) carries a `warnings` reference,
# which CPython's `test_warnings` saves/restores in setUp/tearDown.
import warnings


__all__ = [
    "TestCase",
    "FunctionTestCase",
    "TestSuite",
    "TestLoader",
    "TextTestRunner",
    "TextTestResult",
    "TestResult",
    "main",
    "TestProgram",
    "expectedFailure",
    "skip",
    "skipIf",
    "skipUnless",
    "SkipTest",
    "defaultTestLoader",
    "getTestCaseNames",
    "makeSuite",
    "findTestCases",
    "installHandler",
    "registerResult",
    "removeResult",
    "removeHandler",
    "addModuleCleanup",
    "doModuleCleanups",
    "enterModuleContext",
]


# --------------------------------------------------------------------------
# Module-level cleanups (CPython unittest.case.addModuleCleanup family).
# --------------------------------------------------------------------------

_module_cleanups = []


def addModuleCleanup(function, /, *args, **kwargs):
    """Register *function* to be called on module teardown (LIFO)."""
    _module_cleanups.append((function, args, kwargs))


def enterModuleContext(cm):
    """Enter the supplied context manager and schedule its exit for
    module teardown."""
    result = type(cm).__enter__(cm)
    addModuleCleanup(type(cm).__exit__, cm, None, None, None)
    return result


def doModuleCleanups():
    """Run, in LIFO order, the functions registered with
    ``addModuleCleanup``. Returns a list of ``(exc_type, exc, tb)`` for
    any that raised (mirrors ``doClassCleanups``)."""
    exceptions = []
    while _module_cleanups:
        function, args, kwargs = _module_cleanups.pop()
        try:
            function(*args, **kwargs)
        except Exception:
            exceptions.append(sys.exc_info())
    return exceptions


# --------------------------------------------------------------------------
# Skip / expected-failure machinery
# --------------------------------------------------------------------------

class SkipTest(Exception):
    """Raise inside a test (or pass to ``skipTest``) to skip it."""


class _ShouldStop(Exception):
    """The test run should stop (``--failfast``)."""


class _UnexpectedSuccess(Exception):
    """An ``expectedFailure`` test unexpectedly passed."""


def _id(obj):
    return obj


_subtest_sentinel = object()


def skip(reason):
    """Unconditionally skip the decorated test / class."""
    def decorator(test_item):
        if isinstance(test_item, type):
            test_item.__unittest_skip__ = True
            test_item.__unittest_skip_why__ = reason
            return test_item

        def skip_wrapper(*args, **kwargs):
            raise SkipTest(reason)

        skip_wrapper.__unittest_skip__ = True
        skip_wrapper.__unittest_skip_why__ = reason
        skip_wrapper.__name__ = getattr(test_item, "__name__", "skip_wrapper")
        skip_wrapper.__doc__ = getattr(test_item, "__doc__", None)
        return skip_wrapper

    if isinstance(reason, type):
        # Bare ``@skip`` with no reason.
        return decorator(reason)
    return decorator


def skipIf(condition, reason):
    if condition:
        return skip(reason)
    return _id


def skipUnless(condition, reason):
    if not condition:
        return skip(reason)
    return _id


def expectedFailure(test_item):
    test_item.__unittest_expecting_failure__ = True
    return test_item


# --------------------------------------------------------------------------
# Results
# --------------------------------------------------------------------------

_results = []


def registerResult(result):
    _results.append(result)


def removeResult(result):
    try:
        _results.remove(result)
        return True
    except ValueError:
        return False


def installHandler():
    # SIGINT-aware running isn't modelled; accept the call so
    # ``--catch`` is a no-op rather than an error.
    pass


def removeHandler(method=None):
    if method is not None:
        return method
    return None


class TestResult:
    """Holds the outcome of running a suite of tests."""

    _previousTestClass = None
    _moduleSetUpFailed = False

    def __init__(self, stream=None, descriptions=None, verbosity=None):
        self.failures = []
        self.errors = []
        self.testsRun = 0
        self.skipped = []
        self.expectedFailures = []
        self.unexpectedSuccesses = []
        self.shouldStop = False
        self.buffer = False
        self.failfast = False
        self.tb_locals = False
        self._stdout_buffer = None
        self._stderr_buffer = None
        self._mirrorOutput = False
        self._testRunEntered = False
        self.stream = stream if stream is not None else sys.stderr
        self._verbosity = verbosity or 1
        self.collectedDurations = []

    def printErrors(self):
        pass

    def startTest(self, test):
        self.testsRun += 1
        self._mirrorOutput = False

    def startTestRun(self):
        pass

    def stopTest(self, test):
        pass

    def stopTestRun(self):
        pass

    def addError(self, test, err):
        self.errors.append((test, self._exc_info_to_string(err, test)))
        self._mirrorOutput = True
        if self.failfast:
            self.stop()

    def addFailure(self, test, err):
        self.failures.append((test, self._exc_info_to_string(err, test)))
        self._mirrorOutput = True
        if self.failfast:
            self.stop()

    def addSubTest(self, test, subtest, err):
        if err is not None:
            if issubclass(err[0], test.failureException):
                self.failures.append((subtest, self._exc_info_to_string(err, test)))
            else:
                self.errors.append((subtest, self._exc_info_to_string(err, test)))
            self._mirrorOutput = True
            if self.failfast:
                self.stop()

    def addSuccess(self, test):
        pass

    def addSkip(self, test, reason):
        self.skipped.append((test, reason))

    def addExpectedFailure(self, test, err):
        self.expectedFailures.append((test, self._exc_info_to_string(err, test)))

    def addUnexpectedSuccess(self, test):
        self.unexpectedSuccesses.append(test)

    def addDuration(self, test, elapsed):
        self.collectedDurations.append((str(test), elapsed))

    def wasSuccessful(self):
        return (len(self.failures) == 0
                and len(self.errors) == 0
                and len(self.unexpectedSuccesses) == 0)

    def stop(self):
        self.shouldStop = True

    def _exc_info_to_string(self, err, test):
        try:
            return "".join(_traceback.format_exception(err[0], err[1], err[2]))
        except Exception:
            return "%s: %s\n" % (getattr(err[0], "__name__", err[0]), err[1])

    def __repr__(self):
        return ("<%s run=%i errors=%i failures=%i>"
                % (type(self).__name__, self.testsRun,
                   len(self.errors), len(self.failures)))


# --------------------------------------------------------------------------
# assert* context managers
# --------------------------------------------------------------------------

class _AssertRaisesContext:
    def __init__(self, expected, test_case=None, expected_regex=None):
        self.expected = expected
        self.test_case = test_case
        self.expected_regex = expected_regex
        self.exception = None

    def __enter__(self):
        return self

    def _exc_name(self):
        try:
            return self.expected.__name__
        except AttributeError:
            return str(self.expected)

    def __exit__(self, exc_type, exc_value, tb):
        if exc_type is None:
            raise AssertionError("%s not raised" % self._exc_name())
        if not issubclass(exc_type, self.expected):
            return False
        # CPython detaches the traceback before storing the exception
        # (case.py does `traceback.clear_frames(tb)` plus
        # `with_traceback(None)`): the traceback chain pins every frame
        # between the raise and the handler, and a test that calls
        # `assertRaises` in a loop would otherwise accumulate all of
        # them until the cycle collector runs.
        try:
            import traceback
            traceback.clear_frames(tb)
        except Exception:
            pass
        self.exception = exc_value.with_traceback(None)
        if self.expected_regex is not None:
            import re
            text = str(exc_value)
            if not re.search(self.expected_regex, text):
                raise AssertionError("%r does not match %r"
                                     % (self.expected_regex, text))
        return True


class _AssertWarnsContext:
    def __init__(self, expected, test_case=None, expected_regex=None):
        self.expected = expected
        self.test_case = test_case
        self.expected_regex = expected_regex
        self.warning = None
        self.filename = None
        self.lineno = None

    def __enter__(self):
        import warnings
        self._cm = warnings.catch_warnings(record=True)
        self._records = self._cm.__enter__()
        # CPython exposes the recorded-warnings list as `.warnings`.
        self.warnings = self._records
        warnings.simplefilter("always")
        return self

    def _exc_name(self):
        try:
            return self.expected.__name__
        except AttributeError:
            return str(self.expected)

    def __exit__(self, exc_type, exc_value, tb):
        import warnings
        self._cm.__exit__(exc_type, exc_value, tb)
        if exc_type is not None:
            return False
        first_matching = None
        for w in self._records:
            if not issubclass(w.category, self.expected):
                continue
            if first_matching is None:
                first_matching = w
            if self.expected_regex is not None:
                import re
                if not re.search(self.expected_regex, str(w.message)):
                    continue
            self.warning = w.message
            self.filename = getattr(w, "filename", None)
            self.lineno = getattr(w, "lineno", None)
            return True
        if self.expected_regex is not None and first_matching is not None:
            raise AssertionError("%r does not match %r"
                                 % (self.expected_regex, str(first_matching.message)))
        raise AssertionError("%s not triggered" % self._exc_name())


class _CapturingHandler:
    """Minimal logging handler used by ``assertLogs``."""

    def __init__(self):
        self.records = []
        self.output = []
        self.level = 0

    def flush(self):
        pass

    def handle(self, record):
        self.emit(record)

    def emit(self, record):
        self.records.append(record)
        try:
            msg = record.getMessage()
        except Exception:
            msg = str(getattr(record, "msg", record))
        self.output.append("%s:%s:%s" % (record.levelname, record.name, msg))


class _AssertLogsContext:
    LOGGING_FORMAT = "%(levelname)s:%(name)s:%(message)s"

    def __init__(self, test_case, logger_name, level, no_logs=False):
        self.test_case = test_case
        self.logger_name = logger_name
        self.no_logs = no_logs
        import logging
        if level:
            self.level = logging._nameToLevel.get(level, level) if isinstance(level, str) else level
        else:
            self.level = logging.INFO

    def __enter__(self):
        import logging
        if isinstance(self.logger_name, logging.Logger):
            logger = self.logger = self.logger_name
        else:
            logger = self.logger = logging.getLogger(self.logger_name)
        handler = _CapturingHandler()
        self.watcher = handler
        self.old_handlers = logger.handlers[:]
        self.old_level = logger.level
        self.old_propagate = logger.propagate
        logger.handlers = [handler]
        logger.setLevel(self.level)
        logger.propagate = False
        if self.no_logs:
            return
        return handler

    def __exit__(self, exc_type, exc_value, tb):
        self.logger.handlers = self.old_handlers
        self.logger.propagate = self.old_propagate
        self.logger.setLevel(self.old_level)
        if exc_type is not None:
            return False
        records = self.watcher.records
        if self.no_logs:
            if records:
                raise self.test_case.failureException(
                    "Unexpected logs found: %r" % (self.watcher.output,))
            return
        if not records:
            raise self.test_case.failureException(
                "no logs of level %s or higher triggered on %s"
                % (self.level, getattr(self.logger, "name", self.logger)))


# --------------------------------------------------------------------------
# Sub-tests
# --------------------------------------------------------------------------

class _SubTest:
    def __init__(self, test_case, message, params):
        self.test_case = test_case
        self.message = message
        self.params = params
        self.failureException = test_case.failureException

    def _subDescription(self):
        parts = []
        if self.message is not _subtest_sentinel and self.message is not None:
            parts.append("[%s]" % (self.message,))
        if self.params:
            params_desc = ", ".join(
                "%s=%r" % (k, v) for (k, v) in sorted(self.params.items()))
            parts.append("(%s)" % (params_desc,))
        return " ".join(parts) or "(<subtest>)"

    def id(self):
        return "%s %s" % (self.test_case.id(), self._subDescription())

    def __str__(self):
        return "%s %s" % (self.test_case, self._subDescription())


# --------------------------------------------------------------------------
# TestCase
# --------------------------------------------------------------------------

class TestCase:
    """Base class for all test cases."""

    failureException = AssertionError
    longMessage = True
    maxDiff = 80 * 8
    _classSetupFailed = False
    _class_cleanups = []

    def __init__(self, methodName="runTest"):
        self._testMethodName = methodName
        self._outcome = None
        self._outcome_result = None
        self._subtest = None
        self._cleanups = []
        self._type_equality_funcs = {}
        method = getattr(self, methodName, None)
        if method is None:
            if methodName != "runTest":
                raise ValueError("no such test method in %s: %s"
                                 % (type(self).__name__, methodName))
            self._testMethodDoc = None
        else:
            self._testMethodDoc = method.__doc__

    # -- lifecycle ----------------------------------------------------- #

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

    def skipTest(self, reason):
        raise SkipTest(reason)

    def _callSetUp(self):
        # Indirection point (CPython) so ``IsolatedAsyncioTestCase`` can run
        # ``setUp``/``asyncSetUp`` inside its persistent event loop + context.
        self.setUp()

    def _callTestMethod(self, method):
        # Indirection point CPython uses so ``IsolatedAsyncioTestCase``
        # can drive an ``async def`` test through an event loop. The
        # default just calls the (synchronous) method.
        method()

    def _callTearDown(self):
        self.tearDown()

    def _callCleanup(self, function, /, *args, **kwargs):
        function(*args, **kwargs)

    def shortDescription(self):
        doc = self._testMethodDoc
        return doc.strip().split("\n")[0].strip() if doc else None

    def id(self):
        return "%s.%s.%s" % (type(self).__module__, type(self).__qualname__
                             if hasattr(type(self), "__qualname__")
                             else type(self).__name__,
                             self._testMethodName)

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return self._testMethodName == other._testMethodName

    def __hash__(self):
        return hash((type(self), self._testMethodName))

    def __str__(self):
        return "%s (%s)" % (self._testMethodName, type(self).__name__)

    def __repr__(self):
        cls = type(self)
        return "<%s.%s testMethod=%s>" % (
            cls.__module__, cls.__qualname__, self._testMethodName)

    def countTestCases(self):
        return 1

    def defaultTestResult(self):
        return TestResult()

    # -- cleanups ------------------------------------------------------ #

    def addCleanup(self, function, *args, **kwargs):
        self._cleanups.append((function, args, kwargs))

    @classmethod
    def addClassCleanup(cls, function, *args, **kwargs):
        cls._class_cleanups.append((function, args, kwargs))

    def enterContext(self, cm):
        result = cm.__enter__()
        self.addCleanup(cm.__exit__, None, None, None)
        return result

    @classmethod
    def enterClassContext(cls, cm):
        result = cm.__enter__()
        cls.addClassCleanup(cm.__exit__, None, None, None)
        return result

    def doCleanups(self):
        outcome = True
        while self._cleanups:
            function, args, kwargs = self._cleanups.pop()
            try:
                self._callCleanup(function, *args, **kwargs)
            except KeyboardInterrupt:
                raise
            except Exception:
                outcome = False
                if self._outcome_result is not None:
                    self._outcome_result.addError(self, sys.exc_info())
        return outcome

    @classmethod
    def doClassCleanups(cls):
        errors = []
        while cls._class_cleanups:
            function, args, kwargs = cls._class_cleanups.pop()
            try:
                function(*args, **kwargs)
            except Exception:
                errors.append(sys.exc_info())
        return errors

    # -- sub-tests ----------------------------------------------------- #

    @_contextlib.contextmanager
    def subTest(self, msg=_subtest_sentinel, **params):
        result = self._outcome_result
        if result is None or not hasattr(result, "addSubTest"):
            yield
            return
        parent = self._subtest
        if parent is None:
            params_map = dict(params)
        else:
            params_map = dict(parent.params)
            params_map.update(params)
        self._subtest = _SubTest(self, msg, params_map)
        try:
            yield
        except KeyboardInterrupt:
            raise
        except SkipTest as e:
            result.addSkip(self._subtest, str(e))
        except self.failureException:
            result.addSubTest(self, self._subtest, sys.exc_info())
        except Exception:
            result.addSubTest(self, self._subtest, sys.exc_info())
        else:
            result.addSubTest(self, self._subtest, None)
        finally:
            self._subtest = parent

    # -- running ------------------------------------------------------- #

    def run(self, result=None):
        if result is None:
            result = self.defaultTestResult()
            startTestRun = getattr(result, "startTestRun", None)
            if startTestRun is not None:
                startTestRun()
        self._outcome_result = result
        result.startTest(self)
        try:
            testMethod = getattr(self, self._testMethodName)
            if (getattr(self, "__unittest_skip__", False)
                    or getattr(testMethod, "__unittest_skip__", False)):
                reason = (getattr(testMethod, "__unittest_skip_why__", None)
                          or getattr(self, "__unittest_skip_why__", "")
                          or "skipped")
                result.addSkip(self, reason)
                return result
            expecting_failure = getattr(
                testMethod, "__unittest_expecting_failure__", False)

            # setUp
            try:
                self._callSetUp()
            except SkipTest as e:
                result.addSkip(self, str(e))
                return result
            except KeyboardInterrupt:
                raise
            except Exception:
                result.addError(self, sys.exc_info())
                self.doCleanups()
                return result

            n_fail = len(result.failures)
            n_err = len(result.errors)
            ok = False
            try:
                self._callTestMethod(testMethod)
            except KeyboardInterrupt:
                raise
            except SkipTest as e:
                result.addSkip(self, str(e))
            except self.failureException:
                if expecting_failure:
                    result.addExpectedFailure(self, sys.exc_info())
                else:
                    result.addFailure(self, sys.exc_info())
            except _UnexpectedSuccess:
                result.addUnexpectedSuccess(self)
            except Exception:
                if expecting_failure:
                    result.addExpectedFailure(self, sys.exc_info())
                else:
                    result.addError(self, sys.exc_info())
            else:
                subtest_failed = (len(result.failures) > n_fail
                                  or len(result.errors) > n_err)
                if expecting_failure:
                    if subtest_failed:
                        # The expected failure happened in a subtest.
                        pass
                    else:
                        result.addUnexpectedSuccess(self)
                elif not subtest_failed:
                    ok = True
                    result.addSuccess(self)

            # tearDown
            try:
                self._callTearDown()
            except KeyboardInterrupt:
                raise
            except Exception:
                result.addError(self, sys.exc_info())
                ok = False

            cleanup_ok = self.doCleanups()
            _ = (ok, cleanup_ok)
        finally:
            result.stopTest(self)
            self._outcome_result = None
        return result

    def debug(self):
        self._callSetUp()
        self._callTestMethod(getattr(self, self._testMethodName))
        self._callTearDown()
        while self._cleanups:
            function, args, kwargs = self._cleanups.pop()
            self._callCleanup(function, *args, **kwargs)

    def __call__(self, *args, **kwds):
        return self.run(*args, **kwds)

    # -- assertions ---------------------------------------------------- #

    def _formatMessage(self, msg, standardMsg):
        if not self.longMessage:
            return msg or standardMsg
        if msg is None:
            return standardMsg
        try:
            return "%s : %s" % (standardMsg, msg)
        except Exception:
            return standardMsg

    def fail(self, msg=None):
        raise self.failureException(msg)

    def assertEqual(self, first, second, msg=None):
        if not first == second:
            raise self.failureException(
                self._formatMessage(msg, "%r != %r" % (first, second)))

    def assertNotEqual(self, first, second, msg=None):
        if not first != second:
            raise self.failureException(
                self._formatMessage(msg, "%r == %r" % (first, second)))

    def assertTrue(self, expr, msg=None):
        if not expr:
            raise self.failureException(
                self._formatMessage(msg, "%r is not true" % (expr,)))

    def assertFalse(self, expr, msg=None):
        if expr:
            raise self.failureException(
                self._formatMessage(msg, "%r is not false" % (expr,)))

    def assertIs(self, expr1, expr2, msg=None):
        if expr1 is not expr2:
            raise self.failureException(
                self._formatMessage(msg, "%r is not %r" % (expr1, expr2)))

    def assertIsNot(self, expr1, expr2, msg=None):
        if expr1 is expr2:
            raise self.failureException(
                self._formatMessage(msg, "unexpectedly identical: %r" % (expr1,)))

    def assertIsNone(self, obj, msg=None):
        if obj is not None:
            raise self.failureException(
                self._formatMessage(msg, "%r is not None" % (obj,)))

    def assertIsNotNone(self, obj, msg=None):
        if obj is None:
            raise self.failureException(
                self._formatMessage(msg, "unexpectedly None"))

    def assertIn(self, member, container, msg=None):
        if member not in container:
            raise self.failureException(
                self._formatMessage(msg, "%r not found in %r" % (member, container)))

    def assertNotIn(self, member, container, msg=None):
        if member in container:
            raise self.failureException(
                self._formatMessage(msg, "%r unexpectedly found in %r" % (member, container)))

    def assertIsInstance(self, obj, cls, msg=None):
        if not isinstance(obj, cls):
            raise self.failureException(
                self._formatMessage(msg, "%r is not an instance of %r" % (obj, cls)))

    def assertNotIsInstance(self, obj, cls, msg=None):
        if isinstance(obj, cls):
            raise self.failureException(
                self._formatMessage(msg, "%r is an instance of %r" % (obj, cls)))

    def assertAlmostEqual(self, first, second, places=None, msg=None, delta=None):
        if first == second:
            return
        if delta is not None and places is not None:
            raise TypeError("specify delta or places not both")
        if delta is not None:
            if abs(first - second) <= delta:
                return
            standardMsg = "%r != %r within %r delta (%r difference)" % (
                first, second, delta, abs(first - second))
        else:
            if places is None:
                places = 7
            if round(abs(second - first), places) == 0:
                return
            standardMsg = "%r != %r within %r places" % (first, second, places)
        raise self.failureException(self._formatMessage(msg, standardMsg))

    def assertNotAlmostEqual(self, first, second, places=None, msg=None, delta=None):
        if delta is not None and places is not None:
            raise TypeError("specify delta or places not both")
        diff = abs(first - second)
        if delta is not None:
            if not (first == second) and diff > delta:
                return
            standardMsg = "%r == %r within %r delta (%r difference)" % (
                first, second, delta, diff)
        else:
            if places is None:
                places = 7
            if not (first == second) and round(diff, places) != 0:
                return
            standardMsg = "%r == %r within %r places" % (first, second, places)
        raise self.failureException(self._formatMessage(msg, standardMsg))

    def assertGreater(self, a, b, msg=None):
        if not a > b:
            raise self.failureException(
                self._formatMessage(msg, "%r not greater than %r" % (a, b)))

    def assertGreaterEqual(self, a, b, msg=None):
        if not a >= b:
            raise self.failureException(
                self._formatMessage(msg, "%r not greater than or equal to %r" % (a, b)))

    def assertLess(self, a, b, msg=None):
        if not a < b:
            raise self.failureException(
                self._formatMessage(msg, "%r not less than %r" % (a, b)))

    def assertLessEqual(self, a, b, msg=None):
        if not a <= b:
            raise self.failureException(
                self._formatMessage(msg, "%r not less than or equal to %r" % (a, b)))

    def assertCountEqual(self, first, second, msg=None):
        first_seq = list(first)
        second_seq = list(second)
        if sorted(first_seq, key=repr) != sorted(second_seq, key=repr):
            raise self.failureException(
                self._formatMessage(msg, "Element counts were not equal:\n%r != %r"
                                    % (first_seq, second_seq)))

    def assertSequenceEqual(self, seq1, seq2, msg=None, seq_type=None):
        if seq_type is not None:
            if not isinstance(seq1, seq_type) or not isinstance(seq2, seq_type):
                raise self.failureException(
                    self._formatMessage(msg, "unexpected sequence type"))
        if list(seq1) != list(seq2):
            raise self.failureException(
                self._formatMessage(msg, "%r != %r" % (seq1, seq2)))

    def assertListEqual(self, list1, list2, msg=None):
        self.assertSequenceEqual(list1, list2, msg=msg, seq_type=list)

    def assertTupleEqual(self, tuple1, tuple2, msg=None):
        self.assertSequenceEqual(tuple1, tuple2, msg=msg, seq_type=tuple)

    def assertDictEqual(self, d1, d2, msg=None):
        if not isinstance(d1, dict):
            raise self.failureException("First argument is not a dictionary")
        if not isinstance(d2, dict):
            raise self.failureException("Second argument is not a dictionary")
        if d1 != d2:
            raise self.failureException(
                self._formatMessage(msg, "%r != %r" % (d1, d2)))

    def assertSetEqual(self, set1, set2, msg=None):
        if set(set1) != set(set2):
            raise self.failureException(
                self._formatMessage(msg, "%r != %r" % (set1, set2)))

    def assertMultiLineEqual(self, first, second, msg=None):
        if not isinstance(first, str):
            raise self.failureException("First argument is not a string")
        if not isinstance(second, str):
            raise self.failureException("Second argument is not a string")
        if first != second:
            raise self.failureException(
                self._formatMessage(msg, "%r != %r" % (first, second)))

    def assertRegex(self, text, expected_regex, msg=None):
        import re
        if isinstance(expected_regex, (str, bytes)):
            expected_regex = re.compile(expected_regex)
        if not expected_regex.search(text):
            raise self.failureException(
                self._formatMessage(msg, "Regex didn't match: %r not found in %r"
                                    % (expected_regex.pattern, text)))

    def assertNotRegex(self, text, unexpected_regex, msg=None):
        import re
        if isinstance(unexpected_regex, (str, bytes)):
            unexpected_regex = re.compile(unexpected_regex)
        match = unexpected_regex.search(text)
        if match:
            raise self.failureException(
                self._formatMessage(msg, "Regex matched: %r found in %r"
                                    % (unexpected_regex.pattern, text)))

    def assertRaises(self, expected_exception, *args, **kwargs):
        context = _AssertRaisesContext(expected_exception, self)
        if not args:
            return context
        callable_obj = args[0]
        with context:
            callable_obj(*args[1:], **kwargs)

    def assertRaisesRegex(self, expected_exception, expected_regex, *args, **kwargs):
        context = _AssertRaisesContext(expected_exception, self, expected_regex)
        if not args:
            return context
        callable_obj = args[0]
        with context:
            callable_obj(*args[1:], **kwargs)

    def assertWarns(self, expected_warning, *args, **kwargs):
        context = _AssertWarnsContext(expected_warning, self)
        if not args:
            return context
        callable_obj = args[0]
        with context:
            callable_obj(*args[1:], **kwargs)

    def assertWarnsRegex(self, expected_warning, expected_regex, *args, **kwargs):
        context = _AssertWarnsContext(expected_warning, self, expected_regex)
        if not args:
            return context
        callable_obj = args[0]
        with context:
            callable_obj(*args[1:], **kwargs)

    def assertLogs(self, logger=None, level=None):
        return _AssertLogsContext(self, logger, level, no_logs=False)

    def assertNoLogs(self, logger=None, level=None):
        return _AssertLogsContext(self, logger, level, no_logs=True)

    def assertDictContainsSubset(self, subset, dictionary, msg=None):
        missing = []
        mismatched = []
        for key, value in subset.items():
            if key not in dictionary:
                missing.append(key)
            elif value != dictionary[key]:
                mismatched.append("%r, expected: %r, actual: %r"
                                  % (key, value, dictionary[key]))
        if not (missing or mismatched):
            return
        standardMsg = ""
        if missing:
            standardMsg = "Missing: %r" % (",".join(repr(m) for m in missing),)
        if mismatched:
            if standardMsg:
                standardMsg += "; "
            standardMsg += "Mismatched values: %s" % (",".join(mismatched),)
        raise self.failureException(self._formatMessage(msg, standardMsg))

    # Deprecated aliases kept for source compatibility.
    failUnlessEqual = assertEqual
    failIfEqual = assertNotEqual
    failUnless = assertTrue
    failIf = assertFalse
    failUnlessRaises = assertRaises
    failUnlessAlmostEqual = assertAlmostEqual
    failIfAlmostEqual = assertNotAlmostEqual
    assertEquals = assertEqual
    assertNotEquals = assertNotEqual
    assertAlmostEquals = assertAlmostEqual
    assertNotAlmostEquals = assertNotAlmostEqual
    assert_ = assertTrue


class FunctionTestCase(TestCase):
    """Adapt a plain function to the ``TestCase`` interface."""

    def __init__(self, testFunc, setUp=None, tearDown=None, description=None):
        super().__init__("runTest")
        self._setUpFunc = setUp
        self._tearDownFunc = tearDown
        self._testFunc = testFunc
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

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return (self._setUpFunc == other._setUpFunc
                and self._tearDownFunc == other._tearDownFunc
                and self._testFunc == other._testFunc
                and self._description == other._description)

    def __hash__(self):
        return hash((type(self), self._setUpFunc, self._tearDownFunc,
                     self._testFunc, self._description))

    def shortDescription(self):
        if self._description is not None:
            return self._description
        doc = self._testFunc.__doc__
        return doc and doc.strip().split("\n")[0].strip() or None

    def __str__(self):
        return "%s (%s)" % (type(self).__name__, self._testFunc.__name__)


# --------------------------------------------------------------------------
# TestSuite — with once-per-class / once-per-module fixtures
# --------------------------------------------------------------------------

def _isnotsuite(test):
    try:
        iter(test)
    except TypeError:
        return True
    return not isinstance(test, TestSuite)


def _ismodule(obj):
    return type(obj).__name__ == "module"


class TestSuite:
    def __init__(self, tests=()):
        self._tests = []
        self._removed_tests = 0
        self.addTests(tests)

    def __repr__(self):
        return "<%s tests=%r>" % (type(self).__name__, list(self))

    def __eq__(self, other):
        if not isinstance(other, TestSuite):
            return NotImplemented
        return list(self) == list(other)

    def __iter__(self):
        return iter(self._tests)

    def countTestCases(self):
        cases = self._removed_tests
        for test in self:
            if test:
                cases += test.countTestCases()
        return cases

    def addTest(self, test):
        if not callable(test):
            raise TypeError("%r is not callable" % (repr(test),))
        if (isinstance(test, type)
                and issubclass(test, (TestCase, TestSuite))):
            raise TypeError("TestCases and TestSuites must be instantiated "
                            "before passing them to addTest()")
        self._tests.append(test)

    def addTests(self, tests):
        if isinstance(tests, str):
            raise TypeError("tests must be an iterable of tests, not a string")
        for test in tests:
            self.addTest(test)

    def run(self, result, debug=False):
        topLevel = False
        if not getattr(result, "_testRunEntered", False):
            result._testRunEntered = topLevel = True

        for test in self:
            if result.shouldStop:
                break
            if _isnotsuite(test):
                self._tearDownPreviousClass(test, result)
                self._handleModuleFixture(test, result)
                self._handleClassSetUp(test, result)
                result._previousTestClass = test.__class__
                if (getattr(test.__class__, "_classSetupFailed", False)
                        or getattr(result, "_moduleSetUpFailed", False)):
                    continue
            if not debug:
                test(result)
            else:
                test.debug()

        if topLevel:
            self._tearDownPreviousClass(None, result)
            self._handleModuleTearDown(result)
            result._testRunEntered = False
        return result

    def debug(self):
        debug_result = _DebugResult()
        self.run(debug_result, True)

    def __call__(self, *args, **kwds):
        return self.run(*args, **kwds)

    # -- fixture helpers ---------------------------------------------- #

    def _handleClassSetUp(self, test, result):
        previousClass = getattr(result, "_previousTestClass", None)
        currentClass = test.__class__
        if currentClass == previousClass:
            return
        if getattr(result, "_moduleSetUpFailed", False):
            return
        if getattr(currentClass, "__unittest_skip__", False):
            return
        currentClass._classSetupFailed = False
        setUpClass = getattr(currentClass, "setUpClass", None)
        if setUpClass is not None:
            try:
                setUpClass()
            except KeyboardInterrupt:
                raise
            except Exception:
                currentClass._classSetupFailed = True
                className = currentClass.__name__
                self._createClassOrModuleLevelException(
                    result, sys.exc_info(), "setUpClass", className)

    def _get_previous_module(self, result):
        previousModule = None
        previousClass = getattr(result, "_previousTestClass", None)
        if previousClass is not None:
            previousModule = previousClass.__module__
        return previousModule

    def _handleModuleFixture(self, test, result):
        previousModule = self._get_previous_module(result)
        currentModule = test.__class__.__module__
        if currentModule == previousModule:
            return
        self._handleModuleTearDown(result)
        result._moduleSetUpFailed = False
        module = sys.modules.get(currentModule)
        if module is None:
            return
        setUpModule = getattr(module, "setUpModule", None)
        if setUpModule is not None:
            try:
                setUpModule()
            except KeyboardInterrupt:
                raise
            except Exception:
                result._moduleSetUpFailed = True
                self._createClassOrModuleLevelException(
                    result, sys.exc_info(), "setUpModule", currentModule)

    def _handleModuleTearDown(self, result):
        previousModule = self._get_previous_module(result)
        if previousModule is None:
            return
        if getattr(result, "_moduleSetUpFailed", False):
            return
        module = sys.modules.get(previousModule)
        if module is None:
            return
        tearDownModule = getattr(module, "tearDownModule", None)
        if tearDownModule is not None:
            try:
                tearDownModule()
            except KeyboardInterrupt:
                raise
            except Exception:
                self._createClassOrModuleLevelException(
                    result, sys.exc_info(), "tearDownModule", previousModule)
        # Module-level cleanups run regardless of whether tearDownModule
        # was defined (CPython runs them after tearDownModule).
        for exc_info in doModuleCleanups():
            self._createClassOrModuleLevelException(
                result, exc_info, "moduleCleanUp", previousModule)

    def _tearDownPreviousClass(self, test, result):
        previousClass = getattr(result, "_previousTestClass", None)
        if previousClass is None:
            return
        currentClass = test.__class__ if test is not None else None
        if currentClass == previousClass:
            return
        if getattr(previousClass, "_classSetupFailed", False):
            return
        if getattr(result, "_moduleSetUpFailed", False):
            return
        if getattr(previousClass, "__unittest_skip__", False):
            return
        tearDownClass = getattr(previousClass, "tearDownClass", None)
        if tearDownClass is not None:
            try:
                tearDownClass()
            except KeyboardInterrupt:
                raise
            except Exception:
                className = previousClass.__name__
                self._createClassOrModuleLevelException(
                    result, sys.exc_info(), "tearDownClass", className)
        # Class-level cleanups.
        doClassCleanups = getattr(previousClass, "doClassCleanups", None)
        if doClassCleanups is not None:
            errors = doClassCleanups()
            for exc_info in errors:
                self._createClassOrModuleLevelException(
                    result, exc_info, "classCleanUp", previousClass.__name__)

    def _createClassOrModuleLevelException(self, result, exc_info, method_name, parent):
        error_name = "%s (%s)" % (method_name, parent)
        self._addClassOrModuleLevelException(result, exc_info, error_name)

    def _addClassOrModuleLevelException(self, result, exc_info, errorName):
        error = _ErrorHolder(errorName)
        addError = getattr(result, "addError", None)
        if addError is not None:
            addError(error, exc_info)


class _ErrorHolder:
    """Placeholder ``test`` for a class/module-level error."""

    failureException = None

    def __init__(self, description):
        self.description = description

    def id(self):
        return self.description

    def shortDescription(self):
        return None

    def __repr__(self):
        return "<ErrorHolder description=%r>" % (self.description,)

    def __str__(self):
        return self.id()

    def run(self, result):
        pass

    def __call__(self, result):
        return self.run(result)

    def countTestCases(self):
        return 0


class _DebugResult:
    """Minimal result used by ``TestSuite.debug``."""

    _previousTestClass = None
    _moduleSetUpFailed = False
    shouldStop = False
    _testRunEntered = False


# --------------------------------------------------------------------------
# Loader
# --------------------------------------------------------------------------

def _make_failed_test(methodname, exception, suiteClass):
    def testFailure(self):
        raise exception

    attrs = {methodname: testFailure}
    TestClass = type("ModuleImportFailure", (TestCase,), attrs)
    return suiteClass([TestClass(methodname)])


class TestLoader:
    testMethodPrefix = "test"
    sortTestMethodsUsing = staticmethod(lambda a, b: (a > b) - (a < b))
    suiteClass = TestSuite
    testNamePatterns = None

    def __init__(self):
        self.errors = []
        self._loading_packages = set()

    def loadTestsFromTestCase(self, testCaseClass):
        if issubclass(testCaseClass, TestSuite):
            raise TypeError("Test cases should not be derived from TestSuite.")
        testCaseNames = self.getTestCaseNames(testCaseClass)
        if not testCaseNames and hasattr(testCaseClass, "runTest"):
            testCaseNames = ["runTest"]
        return self.suiteClass([testCaseClass(name) for name in testCaseNames])

    def loadTestsFromModule(self, module, pattern=None):
        tests = []
        for name in dir(module):
            obj = getattr(module, name)
            if isinstance(obj, type) and issubclass(obj, TestCase):
                tests.append(self.loadTestsFromTestCase(obj))
        load_tests = getattr(module, "load_tests", None)
        suite = self.suiteClass(tests)
        if load_tests is not None:
            try:
                return load_tests(self, suite, pattern)
            except Exception as e:
                error_case = _make_failed_test(
                    "load_tests", e, self.suiteClass)
                self.errors.append("Failed to call load_tests:\n%s" % (e,))
                return error_case
        return suite

    def loadTestsFromName(self, name, module=None):
        parts = name.split(".")
        error_case = None
        if module is None:
            parts_copy = parts[:]
            while parts_copy:
                try:
                    module_name = ".".join(parts_copy)
                    module = __import__(module_name)
                    for sub in module_name.split(".")[1:]:
                        module = getattr(module, sub)
                    break
                except ImportError:
                    del parts_copy[-1]
                    if not parts_copy:
                        raise
            parts = parts[len(module_name.split(".")):]
        obj = module
        parent = None
        for part in parts:
            parent, obj = obj, getattr(obj, part)

        if _ismodule(obj):
            return self.loadTestsFromModule(obj)
        elif isinstance(obj, type) and issubclass(obj, TestCase):
            return self.loadTestsFromTestCase(obj)
        elif (isinstance(parent, type) and issubclass(parent, TestCase)
              and callable(obj)):
            name = parts[-1]
            inst = parent(name)
            return self.suiteClass([inst])
        elif isinstance(obj, TestSuite):
            return obj
        elif callable(obj):
            test = obj()
            if isinstance(test, TestSuite):
                return test
            elif isinstance(test, TestCase):
                return self.suiteClass([test])
            else:
                raise TypeError("calling %s returned %s, not a test"
                                % (obj, test))
        else:
            raise TypeError("don't know how to make test from: %s" % (obj,))

    def loadTestsFromNames(self, names, module=None):
        suites = [self.loadTestsFromName(name, module) for name in names]
        return self.suiteClass(suites)

    def getTestCaseNames(self, testCaseClass):
        def shouldIncludeMethod(attrname):
            if not attrname.startswith(self.testMethodPrefix):
                return False
            testFunc = getattr(testCaseClass, attrname, None)
            if not callable(testFunc):
                return False
            return True

        testFnNames = [n for n in dir(testCaseClass) if shouldIncludeMethod(n)]
        import functools
        cmp_to_key = getattr(functools, "cmp_to_key", None)
        if cmp_to_key is not None and self.sortTestMethodsUsing is not None:
            testFnNames.sort(key=cmp_to_key(self.sortTestMethodsUsing))
        else:
            testFnNames.sort()
        return testFnNames

    def discover(self, start_dir, pattern="test*.py", top_level_dir=None):
        import os
        import fnmatch
        suite = self.suiteClass()
        if top_level_dir is None:
            top_level_dir = start_dir
        top_level_dir = os.path.abspath(top_level_dir)
        if top_level_dir not in sys.path:
            sys.path.insert(0, top_level_dir)
        start_dir = os.path.abspath(start_dir)
        names = sorted(os.listdir(start_dir)) if os.path.isdir(start_dir) else []
        for name in names:
            full = os.path.join(start_dir, name)
            if os.path.isfile(full) and fnmatch.fnmatch(name, pattern) and name.endswith(".py"):
                modname = name[:-3]
                rel = os.path.relpath(start_dir, top_level_dir)
                if rel != ".":
                    modname = rel.replace(os.sep, ".") + "." + modname
                try:
                    __import__(modname)
                    module = sys.modules[modname]
                    suite.addTest(self.loadTestsFromModule(module, pattern))
                except Exception as e:
                    self.errors.append("Failed to import %s: %s" % (modname, e))
                    suite.addTest(_make_failed_test(modname, e, self.suiteClass))
            elif os.path.isdir(full) and os.path.isfile(os.path.join(full, "__init__.py")):
                suite.addTest(self.discover(full, pattern, top_level_dir))
        return suite


defaultTestLoader = TestLoader()


def getTestCaseNames(testCaseClass, prefix="test", sortUsing=None, testNamePatterns=None):
    loader = TestLoader()
    loader.testMethodPrefix = prefix
    if sortUsing is not None:
        loader.sortTestMethodsUsing = sortUsing
    return loader.getTestCaseNames(testCaseClass)


def makeSuite(testCaseClass, prefix="test", sortUsing=None, suiteClass=TestSuite):
    loader = TestLoader()
    loader.testMethodPrefix = prefix
    loader.suiteClass = suiteClass
    if sortUsing is not None:
        loader.sortTestMethodsUsing = sortUsing
    return loader.loadTestsFromTestCase(testCaseClass)


def findTestCases(module, prefix="test", sortUsing=None, suiteClass=TestSuite):
    loader = TestLoader()
    loader.testMethodPrefix = prefix
    loader.suiteClass = suiteClass
    if sortUsing is not None:
        loader.sortTestMethodsUsing = sortUsing
    return loader.loadTestsFromModule(module)


# --------------------------------------------------------------------------
# Runner
# --------------------------------------------------------------------------

class TextTestResult(TestResult):
    separator1 = "=" * 70
    separator2 = "-" * 70

    def __init__(self, stream=None, descriptions=True, verbosity=1):
        super().__init__(stream=stream, verbosity=verbosity)
        self.showAll = verbosity > 1
        self.dots = verbosity == 1
        self.descriptions = descriptions
        self.verbosity = verbosity

    def getDescription(self, test):
        doc_first_line = test.shortDescription() if hasattr(test, "shortDescription") else None
        if self.descriptions and doc_first_line:
            return "\n".join((str(test), doc_first_line))
        return str(test)

    def startTest(self, test):
        super().startTest(test)
        if self.showAll:
            self.stream.write(self.getDescription(test))
            self.stream.write(" ... ")

    def addSuccess(self, test):
        super().addSuccess(test)
        if self.showAll:
            self.stream.write("ok\n")
        elif self.dots:
            self.stream.write(".")

    def addError(self, test, err):
        super().addError(test, err)
        if self.showAll:
            self.stream.write("ERROR\n")
        elif self.dots:
            self.stream.write("E")

    def addFailure(self, test, err):
        super().addFailure(test, err)
        if self.showAll:
            self.stream.write("FAIL\n")
        elif self.dots:
            self.stream.write("F")

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        if self.showAll:
            self.stream.write("skipped %r\n" % (reason,))
        elif self.dots:
            self.stream.write("s")

    def addExpectedFailure(self, test, err):
        super().addExpectedFailure(test, err)
        if self.showAll:
            self.stream.write("expected failure\n")
        elif self.dots:
            self.stream.write("x")

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        if self.showAll:
            self.stream.write("unexpected success\n")
        elif self.dots:
            self.stream.write("u")

    def printErrors(self):
        if self.dots or self.showAll:
            self.stream.write("\n")
        self.printErrorList("ERROR", self.errors)
        self.printErrorList("FAIL", self.failures)

    def printErrorList(self, flavour, errors):
        for test, err in errors:
            self.stream.write(self.separator1 + "\n")
            self.stream.write("%s: %s\n" % (flavour, self.getDescription(test)))
            self.stream.write(self.separator2 + "\n")
            self.stream.write("%s\n" % err)


class TextTestRunner:
    resultclass = TextTestResult

    def __init__(self, stream=None, descriptions=True, verbosity=1,
                 failfast=False, buffer=False, resultclass=None, warnings=None,
                 tb_locals=False, durations=None):
        self.stream = stream if stream is not None else sys.stderr
        self.descriptions = descriptions
        self.verbosity = verbosity
        self.failfast = failfast
        self.buffer = buffer
        self.tb_locals = tb_locals
        self.durations = durations
        self.warnings = warnings
        if resultclass is not None:
            self.resultclass = resultclass

    def _makeResult(self):
        return self.resultclass(self.stream, self.descriptions, self.verbosity)

    def run(self, test):
        result = self._makeResult()
        registerResult(result)
        result.failfast = self.failfast
        result.buffer = self.buffer
        startTestRun = getattr(result, "startTestRun", None)
        if startTestRun is not None:
            startTestRun()
        start_time = _time.time()
        try:
            test(result)
        finally:
            stop_time = _time.time()
            stopTestRun = getattr(result, "stopTestRun", None)
            if stopTestRun is not None:
                stopTestRun()
        time_taken = stop_time - start_time
        result.printErrors()
        if self.verbosity:
            self.stream.write(self.resultclass.separator2 + "\n")
        run = result.testsRun
        self.stream.write("Ran %d test%s in %.3fs\n\n"
                          % (run, "" if run == 1 else "s", time_taken))

        expectedFails = len(result.expectedFailures)
        unexpectedSuccesses = len(result.unexpectedSuccesses)
        skipped = len(result.skipped)

        infos = []
        if not result.wasSuccessful():
            self.stream.write("FAILED")
            failed = len(result.failures)
            errored = len(result.errors)
            if failed:
                infos.append("failures=%d" % failed)
            if errored:
                infos.append("errors=%d" % errored)
        else:
            self.stream.write("OK")
        if skipped:
            infos.append("skipped=%d" % skipped)
        if expectedFails:
            infos.append("expected failures=%d" % expectedFails)
        if unexpectedSuccesses:
            infos.append("unexpected successes=%d" % unexpectedSuccesses)
        if infos:
            self.stream.write(" (%s)\n" % (", ".join(infos),))
        else:
            self.stream.write("\n")
        removeResult(result)
        return result


# --------------------------------------------------------------------------
# Program / main
# --------------------------------------------------------------------------

_MAIN_EXAMPLES = """\
Examples:
  weavepy -m unittest test_module               - run tests from test_module
  weavepy -m unittest module.TestClass          - run tests from module.TestClass
  weavepy -m unittest module.Class.test_method  - run a single test method
  weavepy -m unittest discover                  - discover and run all tests
"""


class TestProgram:
    """A command-line program that runs a set of tests (``unittest.main``)."""

    module = None
    verbosity = 1
    failfast = None
    catchbreak = None
    buffer = None
    progName = None
    warnings = None
    testNamePatterns = None

    def __init__(self, module="__main__", defaultTest=None, argv=None,
                 testRunner=None, testLoader=defaultTestLoader, exit=True,
                 verbosity=1, failfast=None, catchbreak=None, buffer=None,
                 warnings=None, tb_locals=False):
        if isinstance(module, str):
            self.module = sys.modules.get(module)
        else:
            self.module = module
        if argv is None:
            argv = sys.argv

        self.exit = exit
        self.failfast = failfast
        self.catchbreak = catchbreak
        self.verbosity = verbosity
        self.buffer = buffer
        self.tb_locals = tb_locals
        self.warnings = warnings
        self.defaultTest = defaultTest
        self.testRunner = testRunner
        self.testLoader = testLoader
        self.progName = "weavepy -m unittest"
        self.parseArgs(argv)
        self.runTests()

    def parseArgs(self, argv):
        self.tests = []
        args = list(argv[1:])
        # ``discover`` sub-command.
        if args and args[0] == "discover":
            self._do_discovery(args[1:])
            return
        rest = []
        i = 0
        while i < len(args):
            a = args[i]
            if a in ("-v", "--verbose"):
                self.verbosity = 2
            elif a in ("-q", "--quiet"):
                self.verbosity = 0
            elif a in ("-f", "--failfast"):
                self.failfast = True
            elif a in ("-c", "--catch"):
                self.catchbreak = True
            elif a in ("-b", "--buffer"):
                self.buffer = True
            elif a in ("-k",):
                i += 1
                if i < len(args):
                    self._add_name_pattern(args[i])
            elif a.startswith("-k"):
                self._add_name_pattern(a[2:])
            elif a in ("-h", "--help"):
                print(_MAIN_EXAMPLES)
                if self.exit:
                    sys.exit(0)
                return
            elif a.startswith("-"):
                # Unknown flag — ignore for forward-compat.
                pass
            else:
                rest.append(a)
            i += 1
        if not rest:
            if self.defaultTest is None:
                self.test = self.testLoader.loadTestsFromModule(self.module)
                return
            elif isinstance(self.defaultTest, str):
                rest = [self.defaultTest]
            else:
                rest = list(self.defaultTest)
        self.testNames = rest
        self.createTests()

    def _add_name_pattern(self, pattern):
        if self.testNamePatterns is None:
            self.testNamePatterns = []
        if "*" not in pattern:
            pattern = "*%s*" % pattern
        self.testNamePatterns.append(pattern)

    def createTests(self):
        self.testLoader.testNamePatterns = self.testNamePatterns
        self.test = self.testLoader.loadTestsFromNames(self.testNames, self.module)

    def _do_discovery(self, argv):
        start_dir = "."
        pattern = "test*.py"
        top_level_dir = None
        positional = []
        i = 0
        while i < len(argv):
            a = argv[i]
            if a in ("-v", "--verbose"):
                self.verbosity = 2
            elif a in ("-s", "--start-directory"):
                i += 1
                start_dir = argv[i]
            elif a in ("-p", "--pattern"):
                i += 1
                pattern = argv[i]
            elif a in ("-t", "--top-level-directory"):
                i += 1
                top_level_dir = argv[i]
            elif not a.startswith("-"):
                positional.append(a)
            i += 1
        if len(positional) >= 1:
            start_dir = positional[0]
        if len(positional) >= 2:
            pattern = positional[1]
        if len(positional) >= 3:
            top_level_dir = positional[2]
        self.test = self.testLoader.discover(start_dir, pattern, top_level_dir)

    def runTests(self):
        if self.catchbreak:
            installHandler()
        if self.testRunner is None:
            self.testRunner = TextTestRunner
        if isinstance(self.testRunner, type):
            try:
                testRunner = self.testRunner(
                    verbosity=self.verbosity,
                    failfast=self.failfast,
                    buffer=self.buffer,
                    warnings=self.warnings,
                    tb_locals=self.tb_locals)
            except TypeError:
                testRunner = self.testRunner(verbosity=self.verbosity)
        else:
            testRunner = self.testRunner
        self.result = testRunner.run(self.test)
        if self.exit:
            sys.exit(0 if self.result.wasSuccessful() else 1)


def main(module="__main__", defaultTest=None, argv=None, testRunner=None,
         testLoader=defaultTestLoader, exit=True, verbosity=1, failfast=None,
         catchbreak=None, buffer=None, warnings=None, tb_locals=False):
    """Run tests. Backward-compatible function wrapper around ``TestProgram``.

    Returns the ``TestResult`` when ``exit=False`` (WeavePy historically
    returned the result here; ``TestProgram`` is also exposed for the
    CPython ``prog.result`` idiom).
    """
    program = TestProgram(
        module=module, defaultTest=defaultTest, argv=argv,
        testRunner=testRunner, testLoader=testLoader, exit=exit,
        verbosity=verbosity, failfast=failfast, catchbreak=catchbreak,
        buffer=buffer, warnings=warnings, tb_locals=tb_locals)
    return getattr(program, "result", None)


# Re-export the async TestCase (CPython does this at the bottom of
# ``unittest/__init__.py``). Done last so ``unittest.TestCase`` is fully
# defined when ``async_case`` imports the package.
from .async_case import IsolatedAsyncioTestCase  # noqa: E402
__all__.append("IsolatedAsyncioTestCase")

# CPython ships ``unittest`` as a package whose submodules (``case``,
# ``result``, ``suite``, ``loader``, ``runner``, ``main``) hold the
# implementation; WeavePy flattens it all into this single module. A few
# stdlib tests reach for those submodules by name (e.g. test_warnings
# saves and restores ``unittest.case.warnings``). Alias the flattened
# module under each submodule name so ``unittest.case.X`` resolves to our
# definitions and ``from unittest.case import Y`` keeps working.
_self_module = sys.modules[__name__]
for _submodule in ("case", "result", "suite", "loader", "runner", "signals", "main"):
    sys.modules.setdefault("unittest." + _submodule, _self_module)
    # Don't clobber a real attribute (e.g. the ``main`` function).
    if not hasattr(_self_module, _submodule):
        setattr(_self_module, _submodule, _self_module)
del _self_module, _submodule
