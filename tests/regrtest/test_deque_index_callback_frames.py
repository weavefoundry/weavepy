"""Index coercion runs with its Python caller's frame published."""
import sys
from collections import deque

observed = []

def rotate_once(queue, index):
    marker = 'rotate-boundary'
    queue.rotate(index)
    return marker


def getitem_once(queue, index):
    marker = 'getitem-boundary'
    result = queue.__getitem__(index)
    return result


class Index:
    def __index__(self):
        caller = sys._getframe(1)
        observed.append((caller.f_code.co_name, caller.f_lineno, caller.f_locals.get('marker')))
        return 1


queue = deque(range(10))
for _ in range(500):
    rotate_once(queue, 1)
    getitem_once(queue, 1)
rotate_once(queue, Index())
getitem_once(queue, Index())
expected = [('rotate_once', rotate_once.__code__.co_firstlineno + 2, 'rotate-boundary'),
            ('getitem_once', getitem_once.__code__.co_firstlineno + 2, 'getitem-boundary')]
assert observed == expected, (observed, expected)

# A declined fast call must not coerce the index before the full call does.
class BrokenIndex:
    def __init__(self, bad_result):
        self.bad_result = bad_result
        self.calls = 0

    def __index__(self):
        self.calls += 1
        caller = sys._getframe(1)
        assert caller.f_code.co_name in ('rotate_once', 'getitem_once')
        assert caller.f_locals['marker'].endswith('-boundary')
        if self.bad_result:
            return 1.5
        raise ValueError('index callback failed')


for call in (rotate_once, getitem_once):
    for bad_result, error in ((True, TypeError), (False, ValueError)):
        value = BrokenIndex(bad_result)
        before = tuple(queue)
        try:
            call(queue, value)
        except error:
            pass
        else:
            raise AssertionError('index error did not propagate')
        assert value.calls == 1
        assert tuple(queue) == before

print('warmed deque callback frame: ok')
