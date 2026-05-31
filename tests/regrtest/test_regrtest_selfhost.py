"""Self-host fixture: exercise the ``test.libregrtest`` runner itself.

RFC 0034 ships a CPython-shaped regression runner behind
``weavepy -m test``. This fixture drives its pieces directly —
argument parsing, the ``State`` classification table, ``TestResult``,
test discovery, the environment-mutation guard, and an end-to-end
``run_single_test`` over *synthetic* test modules written to a temp
dir (one passing, one failing) so we prove the runner grades real
modules correctly.
"""

import os
import sys
import unittest

from test.libregrtest import cmdline
from test.libregrtest import findtests
from test.libregrtest import save_env
from test.libregrtest.result import State, TestResult
from test.libregrtest.single import run_single_test
from test.support import os_helper


class CmdlineTests(unittest.TestCase):
    def test_defaults(self):
        ns = cmdline.parse_args([])
        self.assertEqual(ns.verbose, 0)
        self.assertFalse(ns.quiet)
        self.assertEqual(ns.tests, [])
        self.assertIsNone(ns.use_resources)

    def test_verbose_counts(self):
        ns = cmdline.parse_args(["-v", "-v"])
        self.assertEqual(ns.verbose, 2)

    def test_positional_tests(self):
        ns = cmdline.parse_args(["test_a", "test_b"])
        self.assertEqual(ns.tests, ["test_a", "test_b"])

    def test_resources_all_and_remove(self):
        ns = cmdline.parse_args(["-u", "all"])
        self.assertIn("network", ns.use_resources)
        ns2 = cmdline.parse_args(["-u", "all,-network"])
        self.assertNotIn("network", ns2.use_resources)

    def test_unknown_flag_tolerated(self):
        # CPython compatibility: a verbatim invocation line must not die
        # on a flag we don't model.
        ns = cmdline.parse_args(["--obscure-future-flag", "test_x"])
        self.assertIn("test_x", ns.tests)

    def test_list_flags(self):
        self.assertTrue(cmdline.parse_args(["--list-tests"]).list_tests)
        self.assertTrue(cmdline.parse_args(["--list-cases"]).list_cases)


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
        r = TestResult("test_x", State.ENV_CHANGED)
        self.assertFalse(r.is_failed(fail_env_changed=False))
        self.assertTrue(r.is_failed(fail_env_changed=True))

    def test_str(self):
        r = TestResult("test_x", State.PASSED)
        self.assertEqual(str(r), "test_x: PASSED")


class FindTestsTests(unittest.TestCase):
    def test_findtests_filters_and_sorts(self):
        with os_helper.temp_dir() as path:
            for name in ("test_b.py", "test_a.py", "helper.py",
                         "test_c.txt", "__init__.py"):
                with open(os.path.join(path, name), "w") as fp:
                    fp.write("# stub\n")
            found = findtests.findtests(path)
            self.assertEqual(found, ["test_a", "test_b"])

    def test_exclude(self):
        with os_helper.temp_dir() as path:
            for name in ("test_a.py", "test_b.py"):
                with open(os.path.join(path, name), "w") as fp:
                    fp.write("# stub\n")
            found = findtests.findtests(path, exclude={"test_a"})
            self.assertEqual(found, ["test_b"])


class SaveEnvTests(unittest.TestCase):
    def test_detects_env_mutation(self):
        key = "WEAVEPY_SELFHOST_SAVEENV"
        os.environ.pop(key, None)
        guard = save_env.saved_test_environment("synthetic", 0, True)
        with guard:
            os.environ[key] = "leaked"
        self.assertTrue(guard.changed)
        self.assertNotIn(key, os.environ)

    def test_clean_block_is_unchanged(self):
        guard = save_env.saved_test_environment("synthetic", 0, True)
        with guard:
            pass
        self.assertFalse(guard.changed)


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

_SKIP_SRC = """\
import unittest

class T(unittest.TestCase):
    @unittest.skip("nope")
    def test_skipped(self):
        pass
"""


class RunSingleTestTests(unittest.TestCase):
    def _make_ns(self, testdir):
        ns = cmdline.parse_args([])
        ns.testdir = testdir
        # Quiet so the synthetic failing module's expected output doesn't
        # leak into this fixture's own report.
        ns.quiet = True
        return ns

    def _write(self, path, name, src):
        fn = os.path.join(path, name + ".py")
        with open(fn, "w") as fp:
            fp.write(src)

    def test_passing_module(self):
        with os_helper.temp_dir() as path:
            name = "test_synthetic_pass_%d" % os.getpid()
            self._write(path, name, _PASS_SRC)
            with self._on_path(path):
                result = run_single_test(name, self._make_ns(path))
        self.assertEqual(result.state, State.PASSED)
        self.assertEqual(result.stats[0], 2)  # tests run
        self.assertFalse(result.is_failed())

    def test_failing_module(self):
        with os_helper.temp_dir() as path:
            name = "test_synthetic_fail_%d" % os.getpid()
            self._write(path, name, _FAIL_SRC)
            with self._on_path(path):
                result = run_single_test(name, self._make_ns(path))
        self.assertEqual(result.state, State.FAILED)
        self.assertTrue(result.is_failed())
        self.assertTrue(result.errors)

    def test_skipped_module(self):
        with os_helper.temp_dir() as path:
            name = "test_synthetic_skip_%d" % os.getpid()
            self._write(path, name, _SKIP_SRC)
            with self._on_path(path):
                result = run_single_test(name, self._make_ns(path))
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
