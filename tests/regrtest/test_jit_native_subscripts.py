"""Native subscripts preserve completed values, errors, callbacks, and owners."""

from collections import deque
import gc
import sys
import threading
import weakref


def numeric_reads(values, n):
    total = 0
    for _ in range(n):
        total += values[0] + values[-1]
    return total


def last_read(values, index, n):
    marker = 173
    result = 0
    for _ in range(n):
        result = values[index]
    assert marker == 173
    return result


queue = deque([3, 7])
assert numeric_reads(queue, 4000) == 40000
assert last_read(queue, -1, 4000) == 7
assert last_read(queue, 0, 0) == 0
assert last_read(queue, True, 1) == 7

def conditional_read(values, n):
    marker = 73
    total = 0
    for i in range(n):
        if values:
            total += values[0]
    return total


assert conditional_read(deque([7]), 4000) == 28000

# SUBSCRIPT FALLBACKS:

def fresh_reader():
    namespace = {}
    exec("def reader(values, n):\n"
         "    result = 0\n"
         "    for i in range(n):\n"
         "        result = values[0]\n"
         "    return result\n", namespace)
    reader = namespace["reader"]
    assert reader(deque([11]), 4000) == 11
    return reader


class Payload:
    pass


for value in (False, True, None, 2**80, -(2**80), 1.25, "text", Payload()):
    reader = fresh_reader()
    values = deque([value])
    assert reader(values, 1) is value
    assert reader(values, 5) is value

for index in (2, -3, 2**80):
    try:
        last_read(queue, index, 1)
    except IndexError:
        pass
    else:
        raise AssertionError("missing IndexError")
try:
    fresh_reader()(deque(), 1)
except IndexError as error:
    names = []
    trace = error.__traceback__
    while trace is not None:
        names.append(trace.tb_frame.f_code.co_name)
        trace = trace.tb_next
    assert "reader" in names, names
else:
    raise AssertionError("empty deque read succeeded")

events = []


class Custom(deque):
    pass


custom = Custom([5, 9])
assert last_read(custom, 0, 4000) == 5


def changed_getitem(self, index):
    frame = sys._getframe(1)
    events.append((frame.f_code.co_name, frame.f_locals.get("marker"), index))
    return 29


Custom.__getitem__ = changed_getitem
assert last_read(custom, 0, 3) == 29
assert events == [("last_read", 173, 0)] * 3, events
del Custom.__getitem__
assert last_read(custom, -1, 10) == 9


class Index:
    def __index__(self):
        frame = sys._getframe(1)
        events.append(("index", frame.f_code.co_name, frame.f_locals.get("marker")))
        queue[0] = 31
        return 0


events.clear()
assert last_read(queue, Index(), 3) == 31
assert events == [("index", "last_read", 173)] * 3, events


class Descriptor:
    def __get__(self, instance, owner):
        events.append(("descriptor", instance is not None))
        return lambda index: 41


class Described:
    __getitem__ = Descriptor()


events.clear()
assert last_read(Described(), 0, 3) == 41
assert events == [("descriptor", True)] * 3, events

# An escaped result owns its value; the compiled reader must not own its queue.
reader = fresh_reader()
payload = Payload()
payload_ref = weakref.ref(payload)
values = deque([payload])
queue_ref = weakref.ref(values)
del payload
result = reader(values, 1)
values.clear()
assert payload_ref() is result
del result, values
gc.collect()
assert payload_ref() is None
assert queue_ref() is None

shared = deque([2, 6])
failures = []


def worker():
    try:
        assert numeric_reads(shared, 1000) == 8000
    except BaseException as error:
        failures.append(error)


thread = threading.Thread(target=worker)
thread.start()
thread.join()
assert not failures, failures
assert numeric_reads(shared, 1000) == 8000

# Newly compiled subscript loops must materialize truth callbacks.
events = []

def drain(q, n):
    marker = 73
    total = 0
    i = 0
    while q:
        total += q[0]
        q.popleft()
        i += 1
    return total

class Custom(deque):
    def __bool__(self):
        frame = sys._getframe(1)
        events.append((frame.f_code.co_name, frame.f_locals.get('marker'), frame.f_locals.get('i')))
        return len(self) != 0

assert drain(deque(range(4000)), 4000) == 7998000
assert drain(Custom([1, 2, 3]), 3) == 6
assert events == [('drain', 73, i) for i in range(4)], events

events.clear()
assert conditional_read(Custom([7]), 3) == 21
assert events == [('conditional_read', 73, i) for i in range(3)], events


print("Native subscript semantics: ok")
