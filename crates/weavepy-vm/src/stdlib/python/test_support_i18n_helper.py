"""Minimal `test.support.i18n_helper` shim for WeavePy.

CPython's real helper shells out to `pygettext`/`msgfmt` (via `test.test_tools`)
to verify translation-snapshot freshness — infrastructure WeavePy's bundled run
doesn't carry. We expose the same surface (`TestTranslationsBase`,
`update_translation_snapshots`) so `test_getopt`/`test_optparse` import cleanly;
the two snapshot tests skip, while every other test in those modules runs.
"""

import unittest


class TestTranslationsBase(unittest.TestCase):
    def test_translation_files_exist(self):
        self.skipTest("translation snapshots unavailable under WeavePy")

    def test_translation_snapshots_are_up_to_date(self):
        self.skipTest("translation snapshots unavailable under WeavePy")


def update_translation_snapshots(module):
    raise unittest.SkipTest("translation snapshots unavailable under WeavePy")
