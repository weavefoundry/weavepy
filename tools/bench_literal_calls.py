"""Complete global/static/bound callers with small integer arguments."""
import os
import time

KIND = os.environ.get('WEAVEPY_LITERAL_CALL_KIND', 'global')
if KIND not in ('global', 'static', 'method', 'eight', 'zero'):
    raise ValueError('unknown literal call kind: ' + KIND)


def add(value, amount):
    return value + amount


def eighth(a, b, c, d, e, f, g, h):
    return g + h


def zero():
    return 4


class Rules:
    @staticmethod
    def static_add(value, amount):
        return value + amount
    def method_add(self, value, amount):
        return value + amount


def global_calls(n):
    total = 0
    for i in range(n):
        total += add(i, 3)
    return total


def static_calls(n):
    total = 0
    for i in range(n):
        total += Rules.static_add(i, 3)
    return total


def method_calls(rules, n):
    total = 0
    for i in range(n):
        total += rules.method_add(i, 3)
    return total


def eight_calls(n):
    total = 0
    for i in range(n):
        total += eighth(0, 1, 2, 3, 4, 5, i, 7)
    return total


def zero_calls(n):
    total = 0
    for _ in range(n):
        total += zero()
    return total


def bench(n):
    rules = Rules()
    if KIND == 'global':
        result = global_calls(n)
    elif KIND == 'static':
        result = static_calls(n)
    elif KIND == 'method':
        result = method_calls(rules, n)
    elif KIND == 'eight':
        result = eight_calls(n)
    else:
        result = zero_calls(n)
    expected = 4 * n if KIND == 'zero' else n * (n - 1) // 2 + (7 if KIND == 'eight' else 3) * n
    assert result == expected, (result, expected)
    return result


if __name__ == '__main__':
    n = int(os.environ.get('WEAVEPY_BENCH_WORK', '200000'))
    start = time.perf_counter_ns()
    bench(n)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
