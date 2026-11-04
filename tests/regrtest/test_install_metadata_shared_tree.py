"""The stdlib tree's metadata survives other binaries sharing the tree.

Every binary with the same stdlib shares one cache tree, including symlinks to
it and temporary copies of it. `_sysconfig_vars_*.json` can't depend on which
of them ran last, or `sysconfig` disagrees with its own data file, and running
the same interpreter through a symlink must not rewrite anything.
"""

import _testinternalcapi
import json
import os
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import types
import unittest
from sysconfig.__main__ import _get_json_data_name


class InstallMetadataTests(unittest.TestCase):
    def data_files(self):
        stdlib = sys._stdlib_dir
        return (
            os.path.join(stdlib, _get_json_data_name()),
            os.path.join(stdlib, "build-details.json"),
        )

    def setUp(self):
        for path in self.data_files():
            if not os.path.exists(path):
                self.skipTest("no install metadata in this layout")

    def read(self, path):
        with open(path, encoding="utf-8") as f:
            return f.read()

    def test_bindir_names_the_installation(self):
        path = self.data_files()[0]
        self.assertEqual(
            json.loads(self.read(path))["BINDIR"],
            sysconfig.get_config_var("BINDIR"),
        )
        self.assertEqual(
            sysconfig.get_config_var("BINDIR"),
            os.path.join(sys.base_prefix, "bin"),
        )

    @unittest.skipUnless(hasattr(os, "symlink"), "needs symlinks")
    def test_symlink_leaves_sysconfig_data_alone(self):
        # Only the sysconfig data: a copy running in a parallel sweep may
        # legitimately rewrite build-details.json meanwhile.
        path = self.data_files()[0]
        before = self.read(path)
        with tempfile.TemporaryDirectory() as tmp:
            link = os.path.join(tmp, "python-link")
            os.symlink(os.path.realpath(sys.executable), link)
            subprocess.run([link, "-I", "-S", "-c", "pass"], check=True)
        self.assertEqual(self.read(path), before)

    def test_copy_leaves_sysconfig_data_alone(self):
        path = self.data_files()[0]
        before = self.read(path)
        with tempfile.TemporaryDirectory() as tmp:
            copy = os.path.join(tmp, "python-copy")
            shutil.copy2(os.path.realpath(sys.executable), copy)
            subprocess.run([copy, "-I", "-S", "-c", "pass"], check=True)
            self.assertEqual(self.read(path), before)
        # Hand build-details.json back to this interpreter.
        subprocess.run([sys.executable, "-I", "-S", "-c", "pass"], check=True)


class GetpathTests(unittest.TestCase):
    def test_getpath_code(self):
        code = _testinternalcapi.get_getpath_codeobject()
        self.assertIsInstance(code, types.CodeType)
        self.assertEqual(code.co_filename, "<frozen getpath>")
        self.assertIn("PREFIX", code.co_names)


if __name__ == "__main__":
    unittest.main()
