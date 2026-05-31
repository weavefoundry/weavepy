"""``test.libregrtest.main`` — the regression-test driver.

A faithful subset of CPython 3.13's ``Lib/test/libregrtest/main.py``.
Selects the test modules, runs each through
:func:`test.libregrtest.single.run_single_test`, prints CPython-shaped
progress / summary lines, and returns a process exit code.

Exit codes mirror CPython's ``EXITCODE_*``:

* ``0``   — success (everything passed / was skipped)
* ``1``   — bad command line
* ``2``   — one or more tests failed
* ``3``   — interrupted
* ``4``   — no tests ran
"""

import sys
import time

from test.libregrtest.cmdline import parse_args
from test.libregrtest.findtests import findtestdir, findtests
from test.libregrtest.result import State, TestResult
from test.libregrtest.single import run_single_test


EXITCODE_SUCCESS = 0
EXITCODE_BAD_CMD = 1
EXITCODE_TESTS_FAILED = 2
EXITCODE_INTERRUPTED = 3
EXITCODE_NO_TESTS_RAN = 4


class Regrtest:
    def __init__(self, ns):
        self.ns = ns
        self.testdir = findtestdir(getattr(ns, 'testdir', None))
        self.good = []
        self.bad = []
        self.skipped = []
        self.env_changed = []
        self.run_no_tests = []
        self.interrupted = False
        self.first_state = None
        self.total_tests = 0

    # -- selection --
    def _select_tests(self):
        ns = self.ns
        if ns.fromfile:
            names = self._read_fromfile(ns.fromfile)
        elif ns.tests:
            names = list(ns.tests)
        else:
            names = []

        discovered = findtests(self.testdir)
        if ns.exclude:
            excluded = set(self._normalize(n) for n in names)
            selected = [t for t in discovered
                        if self._normalize(t) not in excluded]
        elif names:
            selected = [self._normalize(n) for n in names]
        else:
            selected = discovered
        return selected

    @staticmethod
    def _normalize(name):
        name = name.strip()
        if name.endswith('.py'):
            name = name[:-3]
        if not name.startswith('test.') and not name.startswith('test_'):
            return name
        if name.startswith('test.'):
            return name.split('.', 1)[1]
        return name

    def _read_fromfile(self, path):
        names = []
        with open(path) as fp:
            for line in fp:
                line = line.split('#', 1)[0].strip()
                if line:
                    names.append(line)
        return names

    def _import_name(self, test_name):
        # Bundled fixtures live in the `test` package; discovered modules
        # on `testdir` import bare. Prefer the package form when it exists.
        if test_name.startswith('test.'):
            return test_name
        return test_name

    # -- run --
    def run_tests(self):
        ns = self.ns
        selected = self._select_tests()
        self.total_tests = len(selected)

        if ns.list_tests:
            for name in selected:
                print(name)
            return EXITCODE_SUCCESS

        if ns.list_cases:
            return self._list_cases(selected)

        if not selected:
            print("No tests selected.")
            return EXITCODE_NO_TESTS_RAN

        start = time.monotonic()
        ran_any = False
        for index, test_name in enumerate(selected, 1):
            if not ns.quiet:
                print("[%d/%d] %s" % (index, self.total_tests, test_name))
                sys.stdout.flush()
            result = run_single_test(self._import_name(test_name), ns)
            ran_any = ran_any or State.has_meaningful_duration(result.state)
            self._accumulate(result)
            if self.first_state is None:
                self.first_state = result.state
            if State.must_stop(result.state):
                self.interrupted = True
                break
            if ns.fail_fast and result.is_failed(ns.fail_env_changed):
                break
        duration = time.monotonic() - start

        self._print_summary(duration)
        return self._exitcode(ran_any)

    def _accumulate(self, result):
        state = result.state
        name = result.test_name
        if state == State.PASSED:
            self.good.append(name)
        elif state == State.ENV_CHANGED:
            self.env_changed.append(name)
        elif state in (State.SKIPPED, State.RESOURCE_DENIED):
            self.skipped.append(name)
        elif state == State.DID_NOT_RUN:
            self.run_no_tests.append(name)
        elif State.is_failed(state):
            self.bad.append((name, result))
        else:
            self.bad.append((name, result))

    def _list_cases(self, selected):
        import importlib
        import unittest
        loader = unittest.TestLoader()
        for test_name in selected:
            try:
                module = importlib.import_module(self._import_name(test_name))
            except Exception:
                continue
            suite = loader.loadTestsFromModule(module)
            for case_id in _iter_case_ids(suite):
                print(case_id)
        return EXITCODE_SUCCESS

    # -- reporting --
    def _print_summary(self, duration):
        print()
        print("== Tests result ==")
        print("Total: %d test module(s) in %.2fs"
              % (self.total_tests, duration))
        if self.good:
            print("  passed: %d" % len(self.good))
        if self.skipped:
            print("  skipped: %d" % len(self.skipped))
        if self.env_changed:
            print("  env changed: %d (%s)"
                  % (len(self.env_changed), ", ".join(self.env_changed)))
        if self.run_no_tests:
            print("  no tests ran: %d" % len(self.run_no_tests))
        if self.bad:
            print("  failed: %d" % len(self.bad))
            for name, result in self.bad:
                print("    %s (%s)" % (name, result.state))
                for case_name, msg in (result.errors or []):
                    first = (msg or "").strip().splitlines()
                    detail = first[-1] if first else ""
                    print("      - %s: %s" % (case_name, detail))
        if self.interrupted:
            print("  INTERRUPTED")
        verdict = "FAILURE" if (self.bad or self.interrupted) else "SUCCESS"
        print("Result: %s" % verdict)

    def _exitcode(self, ran_any):
        if self.interrupted:
            return EXITCODE_INTERRUPTED
        if self.bad:
            return EXITCODE_TESTS_FAILED
        if self.ns.fail_env_changed and self.env_changed:
            return EXITCODE_TESTS_FAILED
        if not ran_any and not self.good:
            return EXITCODE_NO_TESTS_RAN
        return EXITCODE_SUCCESS


def _iter_case_ids(suite):
    import unittest
    for item in suite:
        if isinstance(item, unittest.TestSuite):
            for sub in _iter_case_ids(item):
                yield sub
        else:
            yield item.id()


def main(args=None, **kwargs):
    """Entry point for ``python -m test`` / ``regrtest``."""
    try:
        ns = parse_args(args)
    except SystemExit as exc:
        return exc.code if isinstance(exc.code, int) else EXITCODE_BAD_CMD

    for key, value in kwargs.items():
        setattr(ns, key, value)

    regrtest = Regrtest(ns)
    try:
        return regrtest.run_tests()
    except KeyboardInterrupt:
        print("Interrupted -- exiting", file=sys.stderr)
        return EXITCODE_INTERRUPTED


if __name__ == "__main__":
    sys.exit(main())
