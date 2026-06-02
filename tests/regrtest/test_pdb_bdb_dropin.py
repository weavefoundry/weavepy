"""Drop-in test — `pdb` and `bdb` debugger integration.

RFC 0031 wired the trace dispatch so that ``bdb.Bdb`` and the
``pdb.Pdb`` debugger see the real ``call`` / ``line`` / ``return``
/ ``exception`` event stream. This test exercises the canonical
operations through ``bdb`` (which is what every ``pdb`` command
ultimately boils down to).
"""

import bdb
import pdb
import sys


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label or 'true'))


def test_bdb_runcall_fires_user_hooks():
    events = []

    class Tracing(bdb.Bdb):
        def user_line(self, frame):
            events.append(('line', frame.f_code.co_name))

        def user_call(self, frame, args):
            events.append(('call', frame.f_code.co_name))

        def user_return(self, frame, val):
            events.append(('return', frame.f_code.co_name, val))

        def user_exception(self, frame, exc_info):
            events.append(('exc', frame.f_code.co_name, exc_info[0].__name__))

    def victim():
        x = 1
        y = 2
        return x + y

    dbg = Tracing()
    dbg.runcall(victim)
    line_events = [e for e in events if e[0] == 'line' and e[1] == 'victim']
    return_events = [e for e in events if e[0] == 'return' and e[1] == 'victim']
    assert_true(len(line_events) >= 3, 'at least 3 line events fired')
    assert_eq(len(return_events), 1, 'one return fired')
    assert_eq(return_events[0][2], 3, 'return carries value')


def test_bdb_breakpoint_hits():
    """``set_break`` plus ``runcall`` reports the breakpoint via
    ``break_here`` so debuggers can route to ``user_line``."""
    hits = []

    class BP(bdb.Bdb):
        def user_line(self, frame):
            if self.break_here(frame):
                hits.append((frame.f_code.co_name, frame.f_lineno))

    def victim():
        a = 1
        b = 2
        c = 3
        return a + b + c

    dbg = BP()
    # The function's first body line is `a = 1` — figure out its
    # actual lineno from the code object so the test is robust to
    # source reformatting.
    code = victim.__code__
    target_line = code.co_firstlineno + 2  # body's `b = 2`
    dbg.set_break(code.co_filename, target_line)
    dbg.runcall(victim)
    assert_true(any(l == target_line for _, l in hits),
                'breakpoint at line {} fired'.format(target_line))


def test_bdb_handles_exceptions():
    """``user_exception`` fires when the traced code raises."""
    seen = []

    class ExcBdb(bdb.Bdb):
        def user_exception(self, frame, exc_info):
            seen.append(exc_info[0].__name__)

    def crash():
        raise ValueError('boom')

    dbg = ExcBdb()
    try:
        dbg.runcall(crash)
    except ValueError:
        pass
    assert_true('ValueError' in seen, 'ValueError saw exception event')


def test_pdb_module_loads():
    """`pdb` imports without errors and exposes the canonical API."""
    assert_true(hasattr(pdb, 'set_trace'))
    assert_true(hasattr(pdb, 'post_mortem'))
    assert_true(hasattr(pdb, 'Pdb'))
    assert_true(issubclass(pdb.Pdb, bdb.Bdb))


def test_bdb_clear_break():
    class B(bdb.Bdb):
        pass

    # CPython's ``set_break`` requires the target source line to exist
    # (it consults ``linecache``), so use real lines from a local function
    # rather than a synthetic filename. ``get_all_breaks()`` returns the
    # ``{filename: [lineno, ...]}`` map keyed by file, so assert the
    # per-file line list via ``get_file_breaks``.
    def victim():
        a = 1
        b_ = 2
        return a + b_

    # Breakpoints live in the class-level ``Breakpoint`` registry and a new
    # ``Bdb`` loads them, so clear any left over by earlier tests first
    # (CPython's own test suite does the same in ``setUp``).
    bdb.Breakpoint.clearBreakpoints()
    b = B()
    code = victim.__code__
    fn = code.co_filename
    line1 = code.co_firstlineno + 1  # ``a = 1``
    line2 = code.co_firstlineno + 2  # ``b_ = 2``
    assert_true(b.set_break(fn, line1) is None, 'set_break line1 succeeds')
    assert_true(b.set_break(fn, line2) is None, 'set_break line2 succeeds')
    assert_eq(sorted(b.get_file_breaks(fn)), [line1, line2],
              'two breakpoints registered')
    assert_true(b.get_break(fn, line1) and b.get_break(fn, line2),
                'both breakpoints present')
    b.clear_break(fn, line1)
    assert_true(not b.get_break(fn, line1), 'break at line1 cleared')
    assert_eq(b.get_file_breaks(fn), [line2], 'one breakpoint after clear')


def main():
    tests = [v for k, v in globals().items()
             if k.startswith('test_') and callable(v)]
    failures = 0
    for fn in tests:
        try:
            fn()
            print('OK   {}'.format(fn.__name__))
        except Exception as exc:
            failures += 1
            print('FAIL {}: {}'.format(fn.__name__, exc))
    if failures:
        raise SystemExit(1)
    print('{} pdb/bdb tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
