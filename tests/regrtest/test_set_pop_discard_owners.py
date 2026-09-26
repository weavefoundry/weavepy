"""Discarded set.pop results finalize before their caller resumes."""
import sys
import weakref

events = []

def discard(values):
    marker = 'discard'
    values.pop()
    return len(events)

for i in range(4000):
    assert discard({i}) == 0

class Owner:
    def __del__(self):
        frame = sys._getframe(1)
        events.append((frame.f_code.co_name, frame.f_locals.get('marker')))

for i in range(32):
    values = {Owner()}
    ref = weakref.ref(next(iter(values)))
    count = discard(values)
    assert count == i + 1, (i, count, events)
    assert events[-1] == ('discard', 'discard'), events
    assert ref() is None and not values
print('discarded set.pop ownership: ok')
