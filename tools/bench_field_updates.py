"""Time complete repeated integer-field updates, including setup and checks."""
import os
import time

KIND = os.environ.get('WEAVEPY_FIELD_UPDATE_KIND', 'parameter')
if KIND not in ('parameter', 'constant', 'default', 'slots', 'alias', 'hook'):
    raise ValueError('unknown WEAVEPY_FIELD_UPDATE_KIND: ' + KIND)


class Counter:
    def __init__(self):
        self.value = 0

    def advance(self, amount=2):
        self.value += amount
        return self.value

    def tick(self):
        self.value += 1
        return self.value


class Slots:
    __slots__ = ('value',)
    def __init__(self):
        self.value = 0

    def advance(self, amount=2):
        self.value += amount
        return self.value


class Hook(Counter):
    def __setattr__(self, name, value):
        object.__setattr__(self, name, value)


def run_parameter(counter, n):
    total = 0
    for i in range(n):
        amount = i % 7 - 3
        total += counter.advance(amount)
    return total


def bench(n):
    counter = Slots() if KIND == 'slots' else Hook() if KIND == 'hook' else Counter()
    total = 0
    if KIND == 'constant':
        for _ in range(n):
            total += counter.tick()
        expected, final = n * (n + 1) // 2, n
    elif KIND == 'default':
        for _ in range(n):
            total += counter.advance()
        expected, final = n * (n + 1), 2 * n
    else:
        if KIND == 'alias':
            advance = counter.advance
            for i in range(n):
                total += advance(i % 7 - 3)
        else:
            total = run_parameter(counter, n)
        cycles, tail = divmod(n, 7)
        prefixes = (-3, -5, -6, -6, -5, -3, 0)
        expected = -28 * cycles + sum(prefixes[:tail])
        final = prefixes[tail - 1] if tail else 0
    assert (total, counter.value) == (expected, final), (total, counter.value)
    return total


if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '200000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
