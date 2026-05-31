"""``test.libregrtest`` — WeavePy's regression-test runner package.

A faithful subset of CPython 3.13's ``Lib/test/libregrtest``. The public
entry point is :func:`test.libregrtest.main.main`, re-exported here so
that ``from test.libregrtest import main`` works exactly as it does on
CPython.
"""

from test.libregrtest.main import main, Regrtest
from test.libregrtest.result import State, TestResult
from test.libregrtest.cmdline import parse_args

__all__ = ["main", "Regrtest", "State", "TestResult", "parse_args"]
