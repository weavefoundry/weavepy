"""Drop-in test — `sys.settrace` / `sys.setprofile` observability.

RFC 0031 wires the trace and profile hooks into the VM dispatch
loop so debuggers, coverage tools, and profilers see the full
``call`` / ``line`` / ``return`` / ``exception`` event stream that
CPython produces. This test exercises the canonical contracts:

* ``sys.settrace`` / ``sys.gettrace`` round-trip.
* ``sys.setprofile`` / ``sys.getprofile`` round-trip.
* The trace hook receives ``call`` on frame entry, then a per-frame
  trace function (the hook's return value) is consulted for
  ``line`` / ``return`` / ``exception`` events.
* The profile hook fires once per frame entry / exit and is
  independent of the trace hook.
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
    def trace(frame, event, arg):
        return trace

    prior = sys.gettrace()
    sys.settrace(trace)
    try:
        assert_is(sys.gettrace(), trace, 'gettrace returns the set hook')
    finally:
        sys.settrace(prior)
    assert_eq(sys.gettrace(), prior, 'settrace(None) clears')


def test_setprofile_getprofile_roundtrip():
    def profile(frame, event, arg):
        return profile

    prior = sys.getprofile()
    sys.setprofile(profile)
    try:
        assert_is(sys.getprofile(), profile, 'getprofile returns the set hook')
    finally:
        sys.setprofile(prior)


def test_settrace_fires_line_events():
    """The trace hook fires on every transition between source
    lines inside the traced frame."""
    events = []

    def trace(frame, event, arg):
        events.append((event, frame.f_code.co_name))
        return trace

    def traced():
        x = 1
        y = 2
        return x + y

    sys.settrace(trace)
    try:
        traced()
    finally:
        sys.settrace(None)
    names = [e[0] for e in events if e[1] == 'traced']
    assert_true('call' in names, 'call event fired')
    assert_true(names.count('line') >= 3, 'multiple line events fired')
    assert_true('return' in names, 'return event fired')


def test_settrace_fires_exception_events():
    """An exception raised inside a traced frame fires
    ``'exception'`` before the unwind."""
    saw_exception = []

    def trace(frame, event, arg):
        if event == 'exception':
            saw_exception.append(arg[0].__name__)
        return trace

    def raiser():
        raise ValueError('boom')

    sys.settrace(trace)
    try:
        try:
            raiser()
        except ValueError:
            pass
    finally:
        sys.settrace(None)
    assert_true('ValueError' in saw_exception, 'exception event fired with ValueError')


def test_setprofile_fires_call_return_pairs():
    """The profile hook fires once per frame entry / exit (without
    the line stream)."""
    events = []

    def profile(frame, event, arg):
        events.append((event, frame.f_code.co_name))

    def f():
        return g()

    def g():
        return 1 + 1

    sys.setprofile(profile)
    try:
        f()
    finally:
        sys.setprofile(None)
    profile_events = [e for e in events if e[1] in ('f', 'g')]
    assert_true(('call', 'f') in profile_events, 'profile call f')
    assert_true(('call', 'g') in profile_events, 'profile call g')
    assert_true(('return', 'g') in profile_events, 'profile return g')
    assert_true(('return', 'f') in profile_events, 'profile return f')


def test_tracemalloc_lifecycle():
    tracemalloc.start()
    try:
        assert_true(tracemalloc.is_tracing(), 'start enables tracing')
        # Allocate some objects so the snapshot has rows.
        bag = []
        for i in range(64):
            bag.append([i, i * 2, i * 3])
        current, peak = tracemalloc.get_traced_memory()
        assert_true(current > 0, 'current is positive after allocations')
        assert_true(peak >= current, 'peak >= current')
        snap = tracemalloc.take_snapshot()
        stats = snap.statistics('lineno')
        assert_true(isinstance(stats, list), 'statistics returns a list')
        assert_true(len(stats) > 0, 'statistics has entries')
        # Every stat has count / size / traceback.
        for s in stats[:3]:
            assert_true(s.count >= 1, 'stat.count >= 1')
            assert_true(s.size >= 1, 'stat.size >= 1')
        tracemalloc.clear_traces()
    finally:
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
    sys.monitoring.set_events(0, 0)
    sys.monitoring.free_tool_id(0)
    assert_true(sys.monitoring.get_tool(0) is None)


def test_sys_monitoring_callbacks_fire():
    """PEP 669 — the VM fires registered callbacks at the right
    transitions."""
    events = []

    def on_line(code, line):
        events.append(('LINE', code.co_name, line))

    def on_return(code, offset, retval):
        events.append(('PY_RETURN', code.co_name, retval))

    def on_start(code, offset):
        events.append(('PY_START', code.co_name))

    M = sys.monitoring
    M.use_tool_id(M.PROFILER_ID, 'pep669-test')
    try:
        M.register_callback(M.PROFILER_ID, M.events.PY_START, on_start)
        M.register_callback(M.PROFILER_ID, M.events.PY_RETURN, on_return)
        M.register_callback(M.PROFILER_ID, M.events.LINE, on_line)
        M.set_events(M.PROFILER_ID,
                     M.events.PY_START | M.events.PY_RETURN | M.events.LINE)

        def f():
            v = 11
            return v + 1

        f()
    finally:
        M.set_events(M.PROFILER_ID, 0)
        M.free_tool_id(M.PROFILER_ID)
    py_starts = [e for e in events if e[0] == 'PY_START' and e[1] == 'f']
    py_returns = [e for e in events if e[0] == 'PY_RETURN' and e[1] == 'f']
    lines = [e for e in events if e[0] == 'LINE' and e[1] == 'f']
    assert_eq(len(py_starts), 1, 'one PY_START for f')
    assert_eq(len(py_returns), 1, 'one PY_RETURN for f')
    assert_true(len(lines) >= 2, 'multiple LINE events for f')
    assert_eq(py_returns[0][2], 12, 'PY_RETURN carries retval')


def test_sys_audit_fires_hook():
    """PEP 578 — the registered hook receives stdlib + user-driven
    audit events."""
    captured = []

    def hook(event, args):
        captured.append((event, args))

    sys.addaudithook(hook)
    # User-driven event.
    sys.audit('weavepy.regrtest', 1, 'two', [3])
    # Built-in event from `compile`.
    code = compile('1+1', '<audit>', 'eval')
    # Built-in event from `exec`.
    exec('x_for_audit = 2')
    found_user = any(e[0] == 'weavepy.regrtest' for e in captured)
    found_compile = any(e[0] == 'compile' for e in captured)
    found_exec = any(e[0] == 'exec' for e in captured)
    assert_true(found_user, 'user-driven audit event fired')
    assert_true(found_compile, 'compile audit event fired')
    assert_true(found_exec, 'exec audit event fired')


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
    print('{} settrace/profile/monitoring/audit/tracemalloc tests passed'
          .format(len(tests)))


if __name__ == '__main__':
    main()
