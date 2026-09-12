"""Shared code can specialize different attribute and call shapes concurrently."""

import gc
import threading
import types


class Slotted:
    __slots__ = ('value',)

    def __init__(self, value):
        self.value = value


class Plain:
    def __init__(self, value):
        self.value = value


class Inherited:
    value = 23


module = types.ModuleType('shared_cache_target')
module.value = 31
targets = (Slotted(11), Plain(17), Inherited(), module)
expected = (11, 17, 23, 31)


def read_value(target):
    return target.value


def increment(value):
    return value + 1


def invoke(function, value):
    return function(value)


barrier = threading.Barrier(4)
results = [None] * 4
errors = []


def worker(index):
    try:
        target = targets[index]
        value = expected[index]
        callback = increment if index % 2 else abs
        offset = index % 2
        barrier.wait()
        total = 0
        for i in range(10000):
            observed = read_value(target)
            assert observed == value, (index, observed)
            observed_call = invoke(callback, i)
            assert observed_call == i + offset, (index, observed_call)
            total += observed + observed_call
        results[index] = total
    except BaseException as error:
        errors.append(repr(error))


threads = [threading.Thread(target=worker, args=(index,)) for index in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert not errors, errors
assert results == [49995000 + 10000 * (value + index % 2)
                   for index, value in enumerate(expected)], results
gc.collect()
for target, value in zip(targets, expected):
    assert read_value(target) == value
assert invoke(increment, 5) == 6
assert invoke(abs, -5) == 5
print('shared code cache semantics: ok')
