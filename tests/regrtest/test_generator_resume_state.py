"""Exercise generator resume state across nested and observable activations."""

import gc
import sys
import weakref


def numbers(limit):
    for i in range(limit):
        yield i


def increment(values):
    for value in values:
        yield value + 1


def pipeline(limit, depth):
    values = numbers(limit)
    for _ in range(depth):
        values = increment(values)
    return values


def consume(values):
    total = 0
    for value in values:
        total += value
    return total


for _ in range(4):
    assert consume(pipeline(1200, 6)) == 1200 * 1199 // 2 + 7200
    assert sum(pipeline(1200, 6)) == 1200 * 1199 // 2 + 7200

# A frame materialized between resumes must survive later suspension.
values = numbers(80)
for i in range(20):
    assert next(values) == i
frame = values.gi_frame
assert frame.f_locals["i"] == 19
try:
    frame.clear()
except RuntimeError:
    pass
else:
    raise AssertionError("cleared a suspended generator frame")
assert consume(values) == sum(range(20, 80))
assert values.gi_frame is None
frame.clear()

# A reentrant send sees Running, and its handled error leaves the next yield valid.
owner = []


def reentrant():
    for i in range(150):
        assert owner[0].gi_running
        try:
            owner[0].send(None)
        except ValueError:
            pass
        else:
            raise AssertionError("reentrant generator send succeeded")
        yield i


values = reentrant()
owner.append(values)
assert consume(values) == 150 * 149 // 2
owner.clear()

# Created generators reject non-None sends without consuming their first yield.
def echo():
    value = yield "ready"
    while value is not None:
        value = yield value * 2
    return 41


values = echo()
try:
    values.send(2)
except TypeError:
    pass
else:
    raise AssertionError("sent a value into an unstarted generator")
assert next(values) == "ready"
for i in range(100):
    assert values.send(i) == i * 2
try:
    values.send(None)
except StopIteration as exc:
    assert exc.value == 41
else:
    raise AssertionError("generator did not return")

# Exceptions and close after warm resumes retain handler/finalizer state.
events = []


def guarded():
    try:
        for i in range(150):
            yield i
    except LookupError:
        yield "caught"
    finally:
        events.append("closed")


values = guarded()
for i in range(40):
    assert next(values) == i
assert values.throw(LookupError("probe")) == "caught"
values.close()
assert events == ["closed"]

# A tracing transition observes subsequent resumes and yields.
seen = []


def trace(frame, event, arg):
    if frame.f_code.co_name == "numbers" and event == "return":
        seen.append(arg)
    return trace


values = numbers(35)
for i in range(20):
    assert next(values) == i
sys.settrace(trace)
try:
    for i in range(20, 30):
        assert next(values) == i
finally:
    sys.settrace(None)
assert seen == list(range(20, 30)), seen
assert consume(values) == sum(range(30, 35))

# Nested function calls inside resumes enforce the limit and unwind cleanly.
def recursive_step(depth):
    if depth == 0:
        return 0
    return next(recursive_generator(depth - 1))


def recursive_generator(depth):
    yield recursive_step(depth)


original_limit = sys.getrecursionlimit()
try:
    sys.setrecursionlimit(80)
    try:
        next(recursive_generator(150))
    except RecursionError:
        pass
    else:
        raise AssertionError("recursive generator calls bypassed the recursion limit")
    assert consume(pipeline(20, 3)) == 250
finally:
    sys.setrecursionlimit(original_limit)

# Exhaustion releases a payload retained only by the generator frame.
class Payload:
    pass


def retain_then_yield(payload):
    for i in range(60):
        yield i


payload = Payload()
watched = weakref.ref(payload)
values = retain_then_yield(payload)
del payload
assert watched() is not None
assert consume(values) == 60 * 59 // 2
gc.collect()
assert watched() is None
print("generator resume state: ok")
