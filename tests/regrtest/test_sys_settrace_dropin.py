"""Drop-in test — `sys.settrace` / `sys.setprofile` observability.

The dispatch hook isn't wired into the VM loop yet (gated behind
RFC 0031), so line-level events don't fire — but the *registration*
API has to be observable so debuggers, coverage tools, and
profilers can install themselves without raising.
"""

import sys

import tracemalloc


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label or 'true'))


def assert_is(a, b, label=''):
    if a is not b:
        raise AssertionError('{}: {!r} is not {!r}'.format(label, a, b))


def test_settrace_gettrace_roundtrip():
    def trace(frame, event, arg):  # pragma: no cover - hook isn't fired
        return trace

    prior = sys.gettrace()
    sys.settrace(trace)
    try:
        assert_is(sys.gettrace(), trace, 'gettrace returns the set hook')
    finally:
        sys.settrace(prior)
    assert_eq(sys.gettrace(), prior, 'settrace(None) clears')


def test_setprofile_getprofile_roundtrip():
    def profile(frame, event, arg):  # pragma: no cover
        return profile

    prior = sys.getprofile()
    sys.setprofile(profile)
    try:
        assert_is(sys.getprofile(), profile, 'getprofile returns the set hook')
    finally:
        sys.setprofile(prior)


def test_tracemalloc_lifecycle():
    tracemalloc.start()
    assert_true(tracemalloc.is_tracing(), 'start enables tracing')
    current, peak = tracemalloc.get_traced_memory()
    assert_true(current >= 0, 'current is non-negative')
    assert_true(peak >= 0, 'peak is non-negative')
    snap = tracemalloc.take_snapshot()
    stats = snap.statistics('lineno')
    assert_true(isinstance(stats, list), 'snapshot.statistics returns list')
    tracemalloc.clear_traces()
    tracemalloc.stop()
    assert_true(not tracemalloc.is_tracing(), 'stop disables tracing')


def test_sys_monitoring_constants():
    assert_eq(sys.monitoring.events.NO_EVENTS, 0)
    assert_eq(sys.monitoring.events.LINE, 1 << 7)
    assert_true(hasattr(sys.monitoring, 'use_tool_id'))
    sys.monitoring.use_tool_id(0, 'weavepy-test')
    assert_eq(sys.monitoring.get_tool(0), 'weavepy-test')
    sys.monitoring.set_events(0, sys.monitoring.events.LINE)
    assert_eq(sys.monitoring.get_events(0), sys.monitoring.events.LINE)
    sys.monitoring.free_tool_id(0)
    assert_true(sys.monitoring.get_tool(0) is None)


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
    print('{} debugger/tracemalloc tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
