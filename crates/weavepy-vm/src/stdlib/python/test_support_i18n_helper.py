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

    def assertMsgidsEqual(self, module):
        """Verify a module's gettext msgids match a checked-in snapshot.

        CPython shells out to ``Tools/i18n/pygettext.py`` (gated on
        ``test.test_tools`` + ``requires_subprocess``). WeavePy ships
        neither the i18n tooling nor the snapshot data, so — exactly like
        CPython on a checkout missing those — the check skips.
        """
        self.skipTest("i18n tooling (pygettext) unavailable under WeavePy")


def update_translation_snapshots(module):
    raise unittest.SkipTest("translation snapshots unavailable under WeavePy")
