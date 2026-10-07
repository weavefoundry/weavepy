"""Shared functions and parked generators survive their compiling worker."""
import gc
import threading


def total(n):
    result = 0
    for i in range(n):
        result += i
    return result


def values(n):
    for i in range(n):
        # A non-yielding loop between yields keeps this body eligible for
        # native parking (one before the first yield never runs again).
        warm = 0
        while warm < 1:
            warm += 1
        yield i * 3


parked = []
errors = []


def create():
    try:
        assert total(5000) == 12497500
        # A short first call may defer compilation; the repeated call must
        # still leave compiled code behind when its worker exits.
        assert total(5000) == 12497500
        generator = values(5000)
        # (`send` resumes through the activation that parks natively;
        # `next` may step a simple body in place instead.)
        assert next(generator) == 0
        for i in range(1, 1000):
            assert generator.send(None) == i * 3
        parked.append(generator)
    except BaseException as exc:
        errors.append(exc)


for _ in range(8):
    worker = threading.Thread(target=create)
    worker.start()
    worker.join()
    assert not errors, errors
    assert total(5000) == 12497500
    generator = parked.pop()
    gc.collect()
    for i in range(1000, 5000):
        assert generator.send(None) == i * 3
    try:
        next(generator)
    except StopIteration:
        pass
    else:
        raise AssertionError('exhausted generator kept running')
    del generator
    gc.collect()
print('JIT worker exit: ok')
