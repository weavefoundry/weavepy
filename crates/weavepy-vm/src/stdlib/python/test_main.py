"""``python -m test`` entry point (WeavePy frozen ``test.__main__``).

Dispatches to ``test.libregrtest.main.main`` exactly as CPython's
``Lib/test/__main__.py`` does, propagating its exit code.
"""

from test.libregrtest.main import main

# `_add_python_opts=True`, exactly like CPython's `Lib/test/__main__.py`:
# the CI modes (`--fast-ci`/`--slow-ci`) re-exec the interpreter with
# `-u -W error -bb -E` prepended (test_regrtest's test_add_python_opts
# asserts the re-exec'd worker sees those flags). `main()` never returns.
main(_add_python_opts=True)
