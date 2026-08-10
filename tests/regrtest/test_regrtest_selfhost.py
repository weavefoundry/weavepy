"""Self-host fixture: exercise the ``test.libregrtest`` runner itself.

``weavepy -m test`` is CPython 3.13's *verbatim* ``test.libregrtest``
package (RFC 0060 replaced the earlier WeavePy-shaped runner). This
fixture drives its pieces directly — ``_parse_args``, the ``State``
classification table, ``TestResult``, test discovery, the
environment-mutation guard, and an end-to-end ``run_single_test`` over
*synthetic* test modules written to a temp dir (one passing, one
failing, one skipping) so we prove the runner grades real modules
correctly.
"""

import os
import sys
import unittest

from test.libregrtest import cmdline
from test.libregrtest import findtests
from test.libregrtest import save_env
from test.libregrtest.result import State, TestResult
from test.libregrtest.runtests import RunTests
from test.libregrtest.single import run_single_test
from test.support import os_helper


class CmdlineTests(unittest.TestCase):
    def test_defaults(self):
        ns = cmdline._parse_args([])
        self.assertEqual(ns.verbose, 0)
        self.assertFalse(ns.quiet)
        self.assertEqual(ns.args, [])
        self.assertEqual(ns.use_resources, {})

    def test_verbose_counts(self):
        ns = cmdline._parse_args(["-v", "-v"])
        self.assertEqual(ns.verbose, 2)

    def test_positional_tests(self):
        ns = cmdline._parse_args(["test_a", "test_b"])
        self.assertEqual(ns.args, ["test_a", "test_b"])

    def test_resources_all_and_remove(self):
        ns = cmdline._parse_args(["-u", "all"])
        self.assertIn("network", ns.use_resources)
        ns2 = cmdline._parse_args(["-u", "all,-network"])
        self.assertNotIn("network", ns2.use_resources)

    def test_unknown_flag_rejected(self):
        # The verbatim CPython parser errors out (SystemExit via
        # `parser.error`) on a dash-prefixed argument it doesn't know.
        with self.assertRaises(SystemExit):
            cmdline._parse_args(["--obscure-future-flag", "test_x"])

    def test_list_flags(self):
        self.assertTrue(cmdline._parse_args(["--list-tests"]).list_tests)
        self.assertTrue(cmdline._parse_args(["--list-cases"]).list_cases)


class StateTests(unittest.TestCase):
    def test_is_failed(self):
        self.assertTrue(State.is_failed(State.FAILED))
        self.assertTrue(State.is_failed(State.UNCAUGHT_EXC))
        self.assertFalse(State.is_failed(State.PASSED))
        self.assertFalse(State.is_failed(State.SKIPPED))

    def test_meaningful_duration(self):
        self.assertTrue(State.has_meaningful_duration(State.PASSED))
        self.assertFalse(State.has_meaningful_duration(State.SKIPPED))
        self.assertFalse(State.has_meaningful_duration(State.DID_NOT_RUN))

    def test_must_stop(self):
        self.assertTrue(State.must_stop(State.INTERRUPTED))
        self.assertFalse(State.must_stop(State.FAILED))


class TestResultTests(unittest.TestCase):
    def test_is_failed_env_changed(self):
        r = TestResult("test_x", state=State.ENV_CHANGED)
        self.assertFalse(r.is_failed(fail_env_changed=False))
        self.assertTrue(r.is_failed(fail_env_changed=True))

    def test_str(self):
        r = TestResult("test_x", state=State.PASSED)
        self.assertEqual(str(r), "test_x passed")


class FindTestsTests(unittest.TestCase):
    def test_findtests_filters_and_sorts(self):
        with os_helper.temp_dir() as path:
            for name in ("test_b.py", "test_a.py", "helper.py",
                         "test_c.txt", "__init__.py"):
                with open(os.path.join(path, name), "w") as fp:
                    fp.write("# stub\n")
            found = findtests.findtests(testdir=path)
            self.assertEqual(found, ["test_a", "test_b"])

    def test_exclude(self):
        with os_helper.temp_dir() as path:
            for name in ("test_a.py", "test_b.py"):
                with open(os.path.join(path, name), "w") as fp:
                    fp.write("# stub\n")
            found = findtests.findtests(testdir=path, exclude={"test_a"})
            self.assertEqual(found, ["test_b"])


class SaveEnvTests(unittest.TestCase):
    # The verbatim guard reports a mutation through the
    # `support.environment_altered` flag (and restores the original
    # value), not a `.changed` attribute.

    def test_detects_env_mutation(self):
        from test import support
        key = "WEAVEPY_SELFHOST_SAVEENV"
        os.environ.pop(key, None)
        support.environment_altered = False
        with save_env.saved_test_environment("synthetic", 0, True,
                                             pgo=False):
            os.environ[key] = "leaked"
        self.assertTrue(support.environment_altered)
        self.assertNotIn(key, os.environ)
        support.environment_altered = False

    def test_clean_block_is_unchanged(self):
        from test import support
        support.environment_altered = False
        with save_env.saved_test_environment("synthetic", 0, True,
                                             pgo=False):
            pass
        self.assertFalse(support.environment_altered)


# Synthetic test-module sources run end-to-end through run_single_test.
_PASS_SRC = """\
import unittest

class T(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(1 + 1, 2)
    def test_also_ok(self):
        self.assertTrue(True)
"""

_FAIL_SRC = """\
import unittest

class T(unittest.TestCase):
    def test_bad(self):
        self.assertEqual(1, 2)
"""

# `State.SKIPPED` is a whole-module skip: `unittest.SkipTest` escaping
# module import (a merely-skipped *case* still grades PASSED).
_SKIP_SRC = """\
import unittest
raise unittest.SkipTest("nope")
"""


class RunSingleTestTests(unittest.TestCase):
    def _make_runtests(self, testdir):
        # Quiet so the synthetic failing module's expected output doesn't
        # leak into this fixture's own report.
        return RunTests(
            (),
            fail_fast=False,
            fail_env_changed=False,
            match_tests=[],
            match_tests_dict=None,
            rerun=False,
            forever=False,
            pgo=False,
            pgo_extended=False,
            output_on_failure=False,
            timeout=None,
            verbose=0,
            quiet=True,
            hunt_refleak=None,
            test_dir=testdir,
            use_junit=False,
            coverage=False,
            memory_limit=None,
            gc_threshold=None,
            use_resources={},
            python_cmd=None,
            randomize=False,
            random_seed=0,
        )

    def _write(self, path, name, src):
        fn = os.path.join(path, name + ".py")
        with open(fn, "w") as fp:
            fp.write(src)

    def test_passing_module(self):
        with os_helper.temp_dir() as path:
            name = "test_synthetic_pass_%d" % os.getpid()
            self._write(path, name, _PASS_SRC)
            with self._on_path(path):
                result = run_single_test(name, self._make_runtests(path))
        self.assertEqual(result.state, State.PASSED)
        self.assertEqual(result.stats.tests_run, 2)
        self.assertFalse(result.is_failed(fail_env_changed=False))

    def test_failing_module(self):
        with os_helper.temp_dir() as path:
            name = "test_synthetic_fail_%d" % os.getpid()
            self._write(path, name, _FAIL_SRC)
            with self._on_path(path):
                result = run_single_test(name, self._make_runtests(path))
        self.assertEqual(result.state, State.FAILED)
        self.assertTrue(result.is_failed(fail_env_changed=False))
        self.assertTrue(result.failures)

    def test_skipped_module(self):
        with os_helper.temp_dir() as path:
            name = "test_synthetic_skip_%d" % os.getpid()
            self._write(path, name, _SKIP_SRC)
            with self._on_path(path):
                result = run_single_test(name, self._make_runtests(path))
        self.assertEqual(result.state, State.SKIPPED)

    def _on_path(self, path):
        import contextlib

        @contextlib.contextmanager
        def ctx():
            sys.path.insert(0, path)
            try:
                yield
            finally:
                try:
                    sys.path.remove(path)
                except ValueError:
                    pass
                # Drop synthetic modules so re-runs re-import cleanly.
                for mod in [m for m in sys.modules
                            if m.startswith("test_synthetic_")]:
                    del sys.modules[mod]
        return ctx()


if __name__ == "__main__":
    unittest.main()
