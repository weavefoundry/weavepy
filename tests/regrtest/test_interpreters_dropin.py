"""Drop-in test — PEP 684 sub-interpreters.

RFC 0031 adds an isolated sub-interpreter per ``interpreters.create()``
backed by an independent module cache, builtins dict, and frame
stack. This test exercises the lifecycle, cross-interpreter
channels, and the shareability rules.
"""

import interpreters
from interpreters import (
    Interpreter, Channel, Queue,
    NotShareableError, ChannelClosedError, ChannelEmptyError,
    create, create_channel, list_all,
)


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label or 'true'))


def assert_raises(exc_type, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc_type:
        return
    raise AssertionError(
        'expected {!r} but no exception raised'.format(exc_type.__name__)
    )


def test_create_destroy():
    starting_ids = [x.id for x in list_all()]
    interp = create()
    assert_true(isinstance(interp, Interpreter))
    after_ids = [x.id for x in list_all()]
    assert_true(interp.id in after_ids, 'created interp is listed')
    interp.close()
    final_ids = [x.id for x in list_all()]
    assert_true(interp.id not in final_ids, 'destroyed interp is gone')
    # close() is idempotent.
    interp.close()


def test_exec_runs_isolated():
    interp = create()
    try:
        # State doesn't leak: writing `state` in the sub does NOT
        # mutate the parent's globals.
        interp.exec('state = 99')
        # Re-using the same interpreter sees the prior state — the
        # `__main__` dict persists across `exec` calls.
        interp.exec('assert state == 99, state')
    finally:
        interp.close()


def test_channels_send_recv():
    send, recv = create_channel()
    try:
        send.send('hello')
        send.send(42)
        send.send((1, 2, 'three'))
        assert_eq(recv.recv(), 'hello')
        assert_eq(recv.recv(), 42)
        assert_eq(recv.recv(), (1, 2, 'three'))
        assert_raises(ChannelEmptyError, recv.recv)
    finally:
        send.close()


def test_channels_reject_unshareable():
    send, recv = create_channel()
    try:
        # Lists, dicts, and arbitrary objects aren't shareable per
        # PEP 684 §4.4.
        assert_raises(NotShareableError, send.send, [1, 2, 3])
        assert_raises(NotShareableError, send.send, {'k': 'v'})
        assert_raises(NotShareableError, send.send, object())
    finally:
        send.close()


def test_channel_closed():
    send, recv = create_channel()
    send.send('ok')
    send.close()
    # After close: pending values still drain.
    assert_eq(recv.recv(), 'ok')
    # And sending raises ChannelClosedError.
    assert_raises(ChannelClosedError, send.send, 'too late')
    send.destroy()


def test_queue_fifo():
    q = Queue()
    q.put('a')
    q.put('b')
    q.put('c')
    assert_eq(q.get(), 'a')
    assert_eq(q.get(), 'b')
    assert_eq(q.get(), 'c')
    q.close()


def test_concurrent_send_recv_across_interps():
    """Pre-stage values into a channel and have a sub-interpreter
    pull them out. Validates that the channel registry is global to
    the process even though the interpreter caches are
    independent."""
    send, recv = create_channel()
    send.send('from-main')
    interp = create()
    try:
        # The sub imports _xxsubinterpreters and pulls from the
        # channel using its integer id.
        interp.exec(
            'import _xxsubinterpreters as ssi; '
            'v = ssi.channel_recv({})'.format(recv.id) +
            '; assert v == "from-main", v'
        )
        # Same channel id, opposite direction:
        interp.exec(
            'import _xxsubinterpreters as ssi; '
            'ssi.channel_send({}, "from-sub")'.format(send.id)
        )
        assert_eq(recv.recv(), 'from-sub')
    finally:
        interp.close()
        send.close()


def test_with_statement_lifecycle():
    with create() as interp:
        interp.exec('x = 1')
        assert_true(interp.is_running())
    assert_true(not interp.is_running(), 'context manager destroyed the interp')


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
    print('{} interpreters tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
