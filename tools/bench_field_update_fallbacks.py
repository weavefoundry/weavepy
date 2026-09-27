"""Complete mutable-field calls with additional class and arithmetic fallbacks."""
import os
import time
KIND = os.environ.get('WEAVEPY_FIELD_UPDATE_KIND', 'class-default')

class Counter:
    value = 0
    def __init__(self):
        self.value = 0
    def advance(self, amount):
        self.value += amount
        return self.value

class Plain:
    def __init__(self):
        self.value = 0
    advance = Counter.advance

class Property:
    def __init__(self):
        self._value = 0
    @property
    def value(self):
        return self._value
    @value.setter
    def value(self, value):
        self._value = value
    advance = Counter.advance

class MutableClassValue:
    pass

class WithClassObject(Counter):
    value = MutableClassValue()

def bench(n):
    counter = Plain() if KIND == 'large-integer' else Property() if KIND == 'property' else WithClassObject() if KIND == 'class-object' else Counter()
    initial = (1 << 80) if KIND == 'large-integer' else 0
    counter.value = initial
    total = 0
    for i in range(n):
        amount = i % 7 - 3
        total += counter.advance(amount)
    cycles, tail = divmod(n, 7)
    prefixes = (-3, -5, -6, -6, -5, -3, 0)
    expected = initial * n - 28 * cycles + sum(prefixes[:tail])
    final = initial + (prefixes[tail - 1] if tail else 0)
    assert (total, counter.value) == (expected, final)
    return total

if __name__ == '__main__':
    if KIND not in ['class-default','property','class-object','large-integer']:
        raise ValueError(KIND)
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK','200000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns()-start))
