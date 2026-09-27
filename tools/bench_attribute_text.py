"""Repeated reads of string and bytes attributes in a numeric loop."""

import os
import time


class TextBox:
    def __init__(self, text, data):
        self.text = text
        self.data = data


BOX = TextBox('some text', b'byte content')


def bench(n):
    box = BOX
    total = 0
    for _ in range(n):
        total += len(box.text) + len(box.data)
    assert total == 21 * n
    return total


if __name__ == '__main__':
    n = int(os.environ.get('WEAVEPY_BENCH_WORK', '1000000'))
    start = time.perf_counter_ns()
    bench(n)
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
