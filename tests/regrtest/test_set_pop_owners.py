"""Warm set.pop callers returning heap values with no spare strong owner."""
import gc
import sys
import weakref


def take(values):
    return values.pop()


def warm(n):
    values = set(range(n))
    total = 0
    for _ in range(n):
        total += take(values)
    assert total == n * (n - 1) // 2


warm(4000)
events = []


class Item:
    __slots__ = ('number', '__weakref__')

    def __init__(self, number):
        self.number = number

    def __del__(self):
        events.append(self.number)


def consume(n):
    for i in range(n):
        values = {Item(i)}
        ref = weakref.ref(next(iter(values)))
        popped = take(values)
        assert not values and ref() is popped and popped.number == i
        assert len(events) == i
        del popped
        gc.collect()
        assert ref() is None and len(events) == i + 1
    assert events == list(range(n))


consume(32)
# The cached caller must accept a different heap return without old type assumptions.
values = {('payload', 17)}
returned = take(values)
assert returned == ('payload', 17) and not values
print('warmed heap pop caller: ownership and finalization ok')
