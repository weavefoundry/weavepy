"""Native constant loads share identity and guarded tuple lengths dispatch once."""

import gc
import threading


def literal_string(n):
    value = "cached literal with spaces"
    for _ in range(n):
        value = "cached literal with spaces"
    return value


def literal_name(n):
    value = "interned_literal_name"
    for _ in range(n):
        value = "interned_literal_name"
    return value


def literal_tuple(n):
    value = (None, True, -0.0, 1237940039285380274899124224, "snow 雪", (1, 2), b"data")
    for _ in range(n):
        value = (None, True, -0.0, 1237940039285380274899124224, "snow 雪", (1, 2), b"data")
    return value


def literal_length(n):
    seq = (1, 2, 3)
    total = 0
    for _ in range(n):
        total += len(seq)
    return total


def tuple_lengths(seq, n):
    total = 0
    for _ in range(n):
        total += len(seq)
    return total


def suspended_constants(n):
    value = ("held tuple", (1, 2, 3))
    for _ in range(n):
        yield value


# Capture cold interpreter values before the hot loop and native call
# paths run. Separate activations must retain the same immutable objects.
references = [(function, function(0)) for function in
              (literal_string, literal_name, literal_tuple)]
for _ in range(60):
    for function, reference in references:
        assert function(20) is reference
    assert literal_length(20) == 60
    assert tuple_lengths((1, 2, 3), 20) == 60

# Repeated constant loads must not exhaust the activation's pin table.
for function, reference in references:
    assert function(100000) is reference
assert literal_length(100000) == 300000
assert tuple_lengths((), 20) == 0
assert tuple_lengths(tuple(range(100)), 20) == 2000

generator = suspended_constants(40)
first = next(generator)
gc.collect()
assert all(value is first for value in generator)

calls = []


class TupleSubclass(tuple):
    def __len__(self):
        calls.append("subclass")
        return 7


class CustomLength:
    def __len__(self):
        calls.append("custom")
        return 11


class BrokenLength:
    def __len__(self):
        calls.append("broken")
        raise ValueError("length callback")


# A shape miss must resume before CALL, preserving subclass behavior and
# exactly one callback per iteration, including a callback that raises.
assert tuple_lengths(TupleSubclass((1, 2)), 20) == 140
assert calls == ["subclass"] * 20
calls.clear()
assert tuple_lengths(CustomLength(), 20) == 220
assert calls == ["custom"] * 20
calls.clear()
try:
    tuple_lengths(BrokenLength(), 20)
except ValueError as error:
    assert str(error) == "length callback"
else:
    raise AssertionError("length callback did not raise")
assert calls == ["broken"]
for value in (None, 1, 1.5):
    try:
        tuple_lengths(value, 1)
    except TypeError:
        pass
    else:
        raise AssertionError("non-container length did not raise")


def length_after_callback(seq, n, callback):
    total = 0
    for i in range(n):
        callback(i)
        total += len(seq)
    return total


def no_change(i):
    return i


for _ in range(60):
    assert length_after_callback((1, 2, 3), 20, no_change) == 60


def change_length(i):
    calls.append(i)
    if i == 4:
        globals()["len"] = lambda value: 17


calls.clear()
try:
    assert length_after_callback((1, 2, 3), 20, change_length) == 4 * 3 + 16 * 17
finally:
    globals().pop("len", None)
assert calls == list(range(20))

# Compiled constants and their code table remain shared across threads.
failures = []


def worker():
    try:
        for _ in range(20):
            for function, reference in references:
                assert function(20) is reference
            assert literal_length(20) == 60
    except BaseException as error:
        failures.append(error)


threads = [threading.Thread(target=worker) for _ in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert not failures, failures
print("ok")
