"""Time exact-reference calls and repr with setup and result checks included."""
import gc
import os
import time
import weakref

KIND = os.environ.get('WEAVEPY_WEAKREF_KIND', 'implicit')
if KIND not in ('implicit', 'explicit', 'saved', 'repr', 'explicit-repr', 'saved-repr'):
    raise ValueError('unknown method kind: ' + KIND)

class Owner:
    pass

def bench(n):
    if n < 0:
        raise ValueError('work must be nonnegative')
    owner = Owner()
    reference = weakref.ref(owner)
    total = 0
    if KIND == 'implicit':
        for _ in range(n):
            total += reference() is owner
    elif KIND == 'explicit':
        for _ in range(n):
            total += reference.__call__() is owner
    elif KIND == 'saved':
        call = reference.__call__
        for _ in range(n):
            total += call() is owner
    else:
        if KIND == 'repr':
            sample = repr(reference)
            for _ in range(n):
                total += repr(reference) == sample
        elif KIND == 'explicit-repr':
            sample = reference.__repr__()
            for _ in range(n):
                total += reference.__repr__() == sample
        else:
            call = reference.__repr__
            sample = call()
            for _ in range(n):
                total += call() == sample
        assert sample.startswith('<weakref ') and 'dead' not in sample
    assert total == n
    del owner
    gc.collect()
    assert reference() is None
    return total

if __name__ == '__main__':
    start = time.perf_counter_ns()
    bench(int(os.environ.get('WEAVEPY_BENCH_WORK', '100000')))
    print('WEAVEPY_BENCH_NS=%d' % (time.perf_counter_ns() - start))
