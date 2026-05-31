"""``test.regrtest`` — backwards-compatible shim.

CPython kept ``Lib/test/regrtest.py`` as a thin wrapper after the runner
moved into ``test.libregrtest``. We mirror that: ``python -m
test.regrtest [args]`` and ``from test.regrtest import main`` both route
to :func:`test.libregrtest.main.main`.
"""

import sys

from test.libregrtest.main import main

if __name__ == "__main__":
    sys.exit(main())
