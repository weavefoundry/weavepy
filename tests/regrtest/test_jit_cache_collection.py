"""Return-type memoization must not keep retired caller/callee code alive."""

import gc
import weakref


def exercise(offset, events):
    namespace = {'offset': offset}
    exec('''
def callee(x):
    return x + offset
def caller(n):
    total = 0
    for i in range(n):
        total += callee(i)
    return total
''', namespace)
    callee, caller = namespace['callee'], namespace['caller']
    expected = 10000 * 9999 // 2 + 10000 * offset
    assert caller(10000) == expected
    refs = [weakref.ref(value, lambda ref, tag=tag: events.append(tag))
            for tag, value in enumerate(
                [callee, caller, callee.__code__, caller.__code__])]
    # Live functions continue to protect their code and native dependencies.
    gc.collect()
    assert all(ref() is not None for ref in refs)
    assert not events
    assert caller(10000) == expected
    namespace.pop('callee')
    namespace.pop('caller')
    return refs


for offset in [1, 3, -7, 2.5]:
    events = []
    refs = exercise(offset, events)
    for _ in range(3):
        gc.collect()
    assert all(ref() is None for ref in refs), (offset, [ref() is None for ref in refs], events)
    assert sorted(events) == [0, 1, 2, 3], events
    gc.collect()
    assert sorted(events) == [0, 1, 2, 3], events

print('JIT code cache collection: ok')
