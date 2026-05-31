"""``test.libregrtest.single`` — import, run and classify one test module.

A faithful subset of CPython 3.13's
``Lib/test/libregrtest/single.py``. Builds a suite from a test module
(via ``unittest.defaultTestLoader.loadTestsFromModule``, honouring the
``load_tests`` protocol, falling back to a module-level ``test_main()``),
runs it under :class:`saved_test_environment`, and maps the outcome onto
a :class:`~test.libregrtest.result.State`.
"""

import importlib
import io
import sys
import time
import traceback
import unittest

from test import support
from test.libregrtest.result import State, TestResult
from test.libregrtest.save_env import saved_test_environment


def _ensure_on_path(testdir):
    if testdir and testdir not in sys.path:
        sys.path.insert(0, testdir)


def _build_suite(the_module):
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromModule(the_module)
    try:
        n = suite.countTestCases()
    except Exception:
        n = None
    return suite, n


def _run_suite(suite, verbose, quiet):
    """Run *suite* and return its ``unittest.TestResult``.

    Output is captured into a buffer; verbose runs echo it, quiet runs
    swallow it unless something fails.
    """
    buf = io.StringIO()
    verbosity = 2 if verbose else 1
    runner = unittest.TextTestRunner(stream=buf, verbosity=verbosity)
    result = runner.run(suite)
    output = buf.getvalue()
    if verbose or (not result.wasSuccessful() and not quiet):
        sys.stdout.write(output)
    return result


def _classify(result, env_changed, ran_anything):
    failures = list(getattr(result, 'failures', []))
    errors = list(getattr(result, 'errors', []))
    unexpected = list(getattr(result, 'unexpectedSuccesses', []))
    skipped = list(getattr(result, 'skipped', []))
    run = getattr(result, 'testsRun', 0)

    stats = (run, len(failures), len(errors), len(skipped))

    if failures or errors or unexpected:
        state = State.FAILED
    elif run == 0 and not ran_anything:
        state = State.DID_NOT_RUN
    elif run > 0 and run == len(skipped):
        state = State.SKIPPED
    elif run == 0 and skipped:
        state = State.SKIPPED
    else:
        state = State.PASSED

    if state == State.PASSED and env_changed:
        state = State.ENV_CHANGED

    err_list = [(str(t), msg) for t, msg in (errors + failures)]
    return state, stats, err_list


def _runtest_inner(test_name, ns):
    _ensure_on_path(getattr(ns, 'testdir', None))

    # Reflect runner flags into test.support before importing/running.
    support.verbose = getattr(ns, 'verbose', 0)
    if getattr(ns, 'use_resources', None) is not None:
        support.use_resources = ns.use_resources

    the_module = importlib.import_module(test_name)

    suite, ncases = _build_suite(the_module)
    has_cases = bool(ncases)

    env_changed = False
    with saved_test_environment(test_name, ns.verbose, ns.quiet) as env:
        if has_cases:
            result = _run_suite(suite, ns.verbose, ns.quiet)
            state, stats, errors = _classify(result, env.changed, True)
        elif hasattr(the_module, 'test_main'):
            # Legacy protocol: ``test_main()`` drives the run itself
            # (typically via ``support.run_unittest``) and raises on
            # failure / SkipTest.
            the_module.test_main()
            state, stats, errors = State.PASSED, (0, 0, 0, 0), []
            if env.changed:
                state = State.ENV_CHANGED
        else:
            state, stats, errors = State.DID_NOT_RUN, (0, 0, 0, 0), []
        env_changed = env.changed

    return state, stats, errors


def run_single_test(test_name, ns):
    """Run one test module, returning a :class:`TestResult`."""
    start = time.monotonic()
    result = TestResult(test_name)
    try:
        state, stats, errors = _runtest_inner(test_name, ns)
        result.state = state
        result.stats = stats
        result.errors = errors
    except support.ResourceDenied as exc:
        result.state = State.RESOURCE_DENIED
        result.errors = [(test_name, str(exc))]
    except unittest.SkipTest as exc:
        result.state = State.SKIPPED
        result.errors = [(test_name, str(exc))]
    except KeyboardInterrupt:
        result.state = State.INTERRUPTED
    except BaseException:
        result.state = State.UNCAUGHT_EXC
        result.errors = [(test_name, traceback.format_exc())]
    result.duration = time.monotonic() - start
    return result
