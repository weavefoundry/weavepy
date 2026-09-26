"""Shared native-key caches keep their entries and exact hit/miss counts."""
import functools
import threading


def exercise(maxsize, kind, typed=False):
    @functools.lru_cache(maxsize=maxsize, typed=typed)
    def cached(key):
        return key

    keys = [str(i) if kind == 'str' else
            (i, (str(i), 10**30 + i)) if kind == 'tuple' else i
            for i in range(8)]
    for key in keys:
        assert cached(key) == key
    start = threading.Event()
    errors = []

    def worker(key):
        if not start.wait(10):
            errors.append("start timed out")
            return
        try:
            for _ in range(500):
                if cached(key) != key:
                    errors.append(key)
        except BaseException as error:
            errors.append(error)

    threads = [threading.Thread(target=worker, args=(key,)) for key in keys]
    for thread in threads:
        thread.start()
    start.set()
    for thread in threads:
        thread.join()
    assert not errors, errors
    expected = (0, 4008, 0, 0) if maxsize == 0 else (4000, 8, maxsize, 8)
    assert cached.cache_info() == expected, (maxsize, kind, typed, cached.cache_info())
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, maxsize, 0)


for typed in (False, True):
    for kind in ('int', 'str', 'tuple'):
        for maxsize in (32, 0):
            exercise(maxsize, kind, typed)


def exercise_clear(tuples, typed=False):
    @functools.lru_cache(maxsize=3, typed=typed)
    def cached(key):
        return key

    start = threading.Event()
    errors = []

    def worker(key):
        if not start.wait(10):
            errors.append("start timed out")
            return
        try:
            for number in range(200):
                if key is None:
                    cached.cache_clear()
                else:
                    value = key + number % 7
                    if tuples:
                        value = (value, str(value))
                    if cached(value) != value:
                        errors.append(value)
        except BaseException as error:
            errors.append(error)

    # Each new wrapper exercises initialization racing with the first clear,
    # followed by hits, misses, and evictions racing with subsequent clears.
    threads = [threading.Thread(target=worker, args=(key,)) for key in (None, 0, 10)]
    for thread in threads:
        thread.start()
    start.set()
    for thread in threads:
        thread.join()
    assert not errors, errors
    assert cached.cache_info().currsize <= 3
    cached.cache_clear()
    assert cached.cache_info() == (0, 0, 3, 0)
    for number in range(3):
        key = (number, str(number)) if tuples else number
        assert cached(key) == key
    assert cached.cache_info() == (0, 3, 3, 3)


for _ in range(25):
    exercise_clear(False)
    exercise_clear(True)
    exercise_clear(False, True)
    exercise_clear(True, True)

print("Threaded LRU counters: ok")
