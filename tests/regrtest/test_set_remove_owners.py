"""Removing a native scalar subclass must finalize in the calling frame."""
import gc
import sys

events = []
values = set()


def remove(values, key):
    marker = 'inside remove'
    values.remove(key)
    return len(events)


for n in range(4000):
    assert remove({n}, n) == 0


class Integer(int):
    def __del__(self):
        caller = sys._getframe(1)
        events.append(('int', caller.f_code.co_name, caller.f_locals.get('marker')))
        values.add('finalizer mutation')


class String(str):
    def __del__(self):
        caller = sys._getframe(1)
        events.append(('str', caller.f_code.co_name, caller.f_locals.get('marker')))
        values.add('finalizer mutation')


for cls, key, kind in [(Integer, 3001, 'int'), (String, 'payload', 'str')]:
    values = {cls(key)}
    before = len(events)
    count = remove(values, key)
    assert count == before + 1, (kind, count, events)
    assert events[-1] == (kind, 'remove', 'inside remove'), events
    assert values == {'finalizer mutation'}, values
    gc.collect()
    assert len(events) == before + 1, events
print('set.remove scalar subclass ownership: ok')
