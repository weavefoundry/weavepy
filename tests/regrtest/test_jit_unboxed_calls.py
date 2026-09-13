"""Immediate integer call results preserve completed calls and live aliases."""


def sum_results(n, callback):
    total = 0
    for i in range(n):
        total += callback(i)
    return total


def identity(i):
    return i


for _ in range(60):
    assert sum_results(20, identity) == 190
assert sum_results(100000, identity) == 4999950000


def half_more(i):
    return i + 0.5


old_code = identity.__code__
identity.__code__ = half_more.__code__
assert sum_results(20, identity) == 200.0
identity.__code__ = old_code
assert sum_results(20, identity) == 190

events = []


def observed(i):
    events.append(i)
    return i


assert sum_results(100000, observed) == 4999950000
assert events == list(range(100000))


class Result:
    def __init__(self, value):
        self.value = value

    def __radd__(self, left):
        events.append(self.value)
        return left + self.value


# A compiled constructor returns an instance, which must reach __radd__
# as the completed result instead of being treated as an integer.
for i in range(60):
    assert Result(i).value == i
events.clear()
assert sum_results(20, Result) == 190
assert events == list(range(20))

shared = Result(7)


def return_shared(i):
    return shared


# The returned object can have aliases in globals and in the caller.
# Reading an integer from another return must never retire those pins.
saved = shared
events.clear()
assert sum_results(20, return_shared) == 140
assert events == [7] * 20
assert shared is saved and shared.value == 7


def stable(i):
    return i + 1


def replacement(i):
    return i + 3


def guarded_calls(n, callback):
    total = 0
    for i in range(n):
        total += stable(i)
        total += callback(i)
    return total


for _ in range(60):
    assert guarded_calls(20, identity) == 400


def mutate_global(i):
    global stable
    events.append(i)
    if i == 5:
        stable = replacement
    return i


events.clear()
assert guarded_calls(20, mutate_global) == 428
assert events == list(range(20))
assert sum_results(20, identity) == 190
print("ok")
