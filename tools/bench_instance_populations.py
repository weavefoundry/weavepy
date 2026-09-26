"""Construct, retain, check, and release complete instance populations."""
import os
import time

KIND = os.environ.get('WEAVEPY_INSTANCE_KIND', 'dict-one')
if KIND not in ('empty', 'dict-one', 'dict-eight', 'slot-one', 'slot-eight',
                 'slot-sparse', 'native-int', 'native-str', 'native-tuple'):
    raise ValueError('unknown WEAVEPY_INSTANCE_KIND: ' + KIND)

class Empty:
    pass

class DictOne:
    def __init__(self, value):
        self.value = value

class DictEight:
    def __init__(self, value):
        self.a = value
        self.b = value + 1
        self.c = value + 2
        self.d = value + 3
        self.e = value + 4
        self.f = value + 5
        self.g = value + 6
        self.h = value + 7

class SlotOne:
    __slots__ = ('value',)
    def __init__(self, value):
        self.value = value

class SlotEight:
    __slots__ = ('a', 'b', 'c', 'd', 'e', 'f', 'g', 'h')
    __init__ = DictEight.__init__

class SlotSparse:
    __slots__ = SlotEight.__slots__
    def __init__(self, value):
        self.a = value

class NativeInt(int):
    pass

class NativeStr(str):
    pass

class NativeTuple(tuple):
    pass

def make_empty(value):
    return Empty()

def make_tuple(value):
    return NativeTuple((value, value + 1))

FACTORY = {
    'empty': make_empty,
    'dict-one': DictOne,
    'dict-eight': DictEight,
    'slot-one': SlotOne,
    'slot-eight': SlotEight,
    'slot-sparse': SlotSparse,
    'native-int': NativeInt,
    'native-str': NativeStr,
    'native-tuple': make_tuple,
}[KIND]

def bench(n):
    values = []
    for i in range(n):
        values.append(FACTORY(i))
    assert len(values) == n
    # Selection is outside the construction loop and after full retention.
    if n:
        for i in (0, n // 2, n - 1):
            value = values[i]
            if KIND == 'empty':
                assert type(value) is Empty
            elif KIND in ('dict-one', 'slot-one'):
                assert value.value == i
            elif KIND in ('dict-eight', 'slot-eight'):
                assert (value.a, value.b, value.c, value.d,
                        value.e, value.f, value.g, value.h) == tuple(range(i, i + 8))
            elif KIND == 'slot-sparse':
                assert value.a == i
                assert not hasattr(value, 'b') and not hasattr(value, 'h')
            elif KIND == 'native-int':
                assert type(value) is NativeInt and int(value) == i
            elif KIND == 'native-str':
                assert type(value) is NativeStr and str(value) == str(i)
            else:
                assert type(value) is NativeTuple and tuple(value) == (i, i + 1)
        del value
    del values
    return n

if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
