"""``python -m test`` entry point (WeavePy frozen ``test.__main__``).

Dispatches to ``test.libregrtest.main.main`` exactly as CPython's
``Lib/test/__main__.py`` does, propagating its exit code.
"""

import sys

from test.libregrtest.main import main

sys.exit(main())
