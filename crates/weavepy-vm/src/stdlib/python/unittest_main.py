"""``python -m unittest`` entry point (WeavePy frozen ``unittest.__main__``)."""

import sys
import unittest

if sys.argv and sys.argv[0].endswith("__main__.py"):
    sys.argv[0] = "weavepy -m unittest"

__unittest = True

unittest.main(module=None)
