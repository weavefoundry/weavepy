"""Logger level methods on a disabled level return at once.

`logger.debug(...)` and its kin, for a level the logger is disabled for as
its class's `isEnabledFor` decides it, return `None` without the Python
method; everything else (an enabled level, keywords, a subclass's own
`isEnabledFor`, `logging.disable`, a changed level) behaves as before,
records and their caller information included.
"""

import logging
import unittest


class ListHandler(logging.Handler):
    def __init__(self):
        super().__init__()
        self.records = []

    def emit(self, record):
        self.records.append(record)


class LoggingDisabledTests(unittest.TestCase):
    def setUp(self):
        self.logger = logging.getLogger("weave.test.disabled")
        self.logger.propagate = False
        self.handler = ListHandler()
        self.logger.addHandler(self.handler)
        self.logger.setLevel(logging.WARNING)

    def tearDown(self):
        self.logger.removeHandler(self.handler)
        self.logger.disabled = False
        logging.disable(logging.NOTSET)

    def test_disabled_levels_drop(self):
        for i in range(500):
            self.assertIsNone(self.logger.debug("x %s", i))
            self.assertIsNone(self.logger.info("y"))
        self.assertEqual(self.handler.records, [])
        self.logger.warning("w %d", 1)
        self.assertEqual([r.getMessage() for r in self.handler.records], ["w 1"])

    def test_level_changes(self):
        for i in range(100):
            self.logger.debug("drop")
        self.logger.setLevel(logging.DEBUG)
        self.logger.debug("keep %s", "me")
        self.assertEqual([r.getMessage() for r in self.handler.records], ["keep me"])
        rec = self.handler.records[0]
        self.assertEqual(rec.funcName, "test_level_changes")
        self.assertTrue(rec.pathname.endswith("test_logging_disabled_levels.py"))
        self.logger.disabled = True
        for i in range(100):
            self.logger.error("drop")
        self.logger.disabled = False
        logging.disable(logging.ERROR)
        for i in range(100):
            self.logger.error("drop")
        logging.disable(logging.NOTSET)
        self.logger.critical("c")
        self.assertEqual([r.getMessage() for r in self.handler.records], ["keep me", "c"])

    def test_keywords_and_stacklevel(self):
        self.logger.setLevel(logging.INFO)

        def helper():
            self.logger.info("from helper", stacklevel=2)

        helper()
        self.assertEqual(self.handler.records[-1].funcName, "test_keywords_and_stacklevel")
        for i in range(50):
            self.logger.debug("drop", exc_info=True)
        self.assertEqual(len(self.handler.records), 1)
        try:
            raise ValueError("boom")
        except ValueError:
            self.logger.error("caught", exc_info=True)
        self.assertIsNotNone(self.handler.records[-1].exc_info)

    def test_subclass_and_adapter(self):
        class Loud(logging.Logger):
            def isEnabledFor(self, level):
                return True

        loud = Loud("weave.loud")
        h = ListHandler()
        loud.addHandler(h)
        loud.setLevel(logging.CRITICAL)
        for i in range(50):
            loud.debug("always %d", i)
        self.assertEqual(len(h.records), 50)
        adapter = logging.LoggerAdapter(self.logger, {"k": 1})
        for i in range(50):
            adapter.debug("drop")
        adapter.warning("kept")
        self.assertEqual(self.handler.records[-1].getMessage(), "kept")


if __name__ == "__main__":
    unittest.main()
