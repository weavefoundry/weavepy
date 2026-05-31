"""Self-host fixture: exercise ``test.support`` and its helper submodules.

Runs as a plain ``unittest`` module so the bundled regrtest runner (which
executes the file and checks the exit code) and ``weavepy -m test`` (which
imports it and loads the cases) both grade it. This is the RFC 0034
contract: the helper layer every CPython ``Lib/test/test_*.py`` imports
must actually work on WeavePy.
"""

import os
import sys
import unittest
import warnings

from test import support
from test.support import os_helper
from test.support import import_helper
from test.support import warnings_helper


class CapturedIOTests(unittest.TestCase):
    def test_captured_stdout(self):
        with support.captured_stdout() as out:
            print("hello-capture")
        self.assertEqual(out.getvalue(), "hello-capture\n")

    def test_captured_stderr(self):
        with support.captured_stderr() as err:
            print("err-line", file=sys.stderr)
        self.assertIn("err-line", err.getvalue())

    def test_captured_output_restores_stream(self):
        original = sys.stdout
        with support.captured_stdout():
            pass
        self.assertIs(sys.stdout, original)


class SwapTests(unittest.TestCase):
    def test_swap_attr_existing(self):
        class Box:
            value = 1
        with support.swap_attr(Box, "value", 99) as old:
            self.assertEqual(old, 1)
            self.assertEqual(Box.value, 99)
        self.assertEqual(Box.value, 1)

    def test_swap_attr_missing_is_removed(self):
        class Box:
            pass
        obj = Box()
        with support.swap_attr(obj, "added", 5):
            self.assertEqual(obj.added, 5)
        self.assertFalse(hasattr(obj, "added"))

    def test_swap_item(self):
        d = {"a": 1}
        with support.swap_item(d, "a", 2) as old:
            self.assertEqual(old, 1)
            self.assertEqual(d["a"], 2)
        self.assertEqual(d["a"], 1)
        with support.swap_item(d, "b", 3):
            self.assertEqual(d["b"], 3)
        self.assertNotIn("b", d)


class GCHelperTests(unittest.TestCase):
    def test_gc_collect_runs(self):
        support.gc_collect()

    def test_disable_gc_restores(self):
        import gc
        was_enabled = gc.isenabled()
        with support.disable_gc():
            self.assertFalse(gc.isenabled())
        self.assertEqual(gc.isenabled(), was_enabled)


class ImplDetailTests(unittest.TestCase):
    def test_check_impl_detail_for_weavepy(self):
        # WeavePy is *not* CPython, so a cpython=True guard is False and
        # CPython-internal tests skip honestly instead of failing.
        self.assertFalse(support.check_impl_detail(cpython=True))
        self.assertTrue(support.check_impl_detail(cpython=False))

    def test_check_impl_detail_default(self):
        # With no guards the default guard is ``cpython=True``; on WeavePy
        # that resolves the same way as an explicit cpython guard.
        self.assertEqual(support.check_impl_detail(),
                         support.check_impl_detail(cpython=True))


class ResourceGateTests(unittest.TestCase):
    def test_resource_disabled_by_default(self):
        self.assertFalse(support.is_resource_enabled("network"))

    def test_requires_raises_resource_denied(self):
        with self.assertRaises(support.ResourceDenied):
            support.requires("network")

    def test_resource_denied_is_skiptest(self):
        self.assertTrue(issubclass(support.ResourceDenied,
                                   unittest.SkipTest))


class SentinelTests(unittest.TestCase):
    def test_always_eq(self):
        self.assertEqual(support.ALWAYS_EQ, object())
        self.assertEqual(support.ALWAYS_EQ, 12345)

    def test_never_eq(self):
        self.assertNotEqual(support.NEVER_EQ, support.NEVER_EQ)

    def test_largest_smallest(self):
        self.assertGreater(support.LARGEST, 10 ** 9)
        self.assertLess(support.SMALLEST, -(10 ** 9))
        self.assertGreater(support.LARGEST, support.SMALLEST)


class MiscTests(unittest.TestCase):
    def test_sortdict(self):
        self.assertEqual(support.sortdict({"b": 2, "a": 1}),
                         "{'a': 1, 'b': 2}")

    def test_findfile_passthrough(self):
        # An unknown file comes back unchanged rather than raising.
        self.assertEqual(support.findfile("does_not_exist_zzz.dat"),
                         "does_not_exist_zzz.dat")


class EnvironmentVarGuardTests(unittest.TestCase):
    def test_set_and_restore(self):
        key = "WEAVEPY_SELFHOST_ENV"
        os.environ.pop(key, None)
        with os_helper.EnvironmentVarGuard() as env:
            env.set(key, "abc")
            self.assertEqual(os.environ[key], "abc")
        self.assertNotIn(key, os.environ)

    def test_restore_prior_value(self):
        key = "WEAVEPY_SELFHOST_ENV2"
        os.environ[key] = "orig"
        try:
            with os_helper.EnvironmentVarGuard() as env:
                env.set(key, "temp")
                self.assertEqual(os.environ[key], "temp")
            self.assertEqual(os.environ[key], "orig")
        finally:
            os.environ.pop(key, None)

    def test_unset(self):
        key = "WEAVEPY_SELFHOST_ENV3"
        os.environ[key] = "x"
        with os_helper.EnvironmentVarGuard() as env:
            env.unset(key)
            self.assertNotIn(key, os.environ)
        self.assertEqual(os.environ.get(key), "x")
        os.environ.pop(key, None)


class TempDirTests(unittest.TestCase):
    def test_temp_dir_created_and_removed(self):
        with os_helper.temp_dir() as path:
            self.assertTrue(os.path.isdir(path))
            marker = os.path.join(path, "marker.txt")
            with open(marker, "w") as fp:
                fp.write("data")
            self.assertTrue(os.path.exists(marker))
        self.assertFalse(os.path.exists(path))

    def test_temp_cwd(self):
        outer = os.getcwd()
        with os_helper.temp_cwd() as path:
            self.assertEqual(os.path.realpath(os.getcwd()),
                             os.path.realpath(path))
        self.assertEqual(os.getcwd(), outer)

    def test_unlink_missing_is_silent(self):
        # Removing a non-existent file must not raise.
        os_helper.unlink(os.path.join(os.getcwd(), "no_such_file_zzz.tmp"))


class ImportHelperTests(unittest.TestCase):
    def test_import_module_returns_module(self):
        mod = import_helper.import_module("math")
        self.assertTrue(hasattr(mod, "sqrt"))

    def test_import_module_missing_skips(self):
        with self.assertRaises(unittest.SkipTest):
            import_helper.import_module("a_module_that_does_not_exist_zzz")

    def test_dirs_on_syspath(self):
        before = list(sys.path)
        with import_helper.DirsOnSysPath(os.getcwd()):
            self.assertIn(os.getcwd(), sys.path)
        self.assertEqual(sys.path, before)


class WarningsHelperTests(unittest.TestCase):
    def test_check_warnings_records(self):
        import warnings
        with warnings_helper.check_warnings(quiet=True) as w:
            warnings.warn("a deprecation", DeprecationWarning)
        self.assertTrue(any("deprecation" in str(rec.message)
                            for rec in w.warnings))

    @warnings_helper.ignore_warnings(category=DeprecationWarning)
    def _emit_ignored(self):
        warnings.warn("ignored", DeprecationWarning)
        return 42

    def test_ignore_warnings_decorator(self):
        # ``ignore_warnings`` is a *method* decorator (it forwards ``self``).
        self.assertEqual(self._emit_ignored(), 42)


if __name__ == "__main__":
    unittest.main()
