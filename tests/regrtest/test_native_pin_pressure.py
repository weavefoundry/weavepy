"""Preserve live state and callback counts across native loop cleanup."""

import gc


def text_loop(n):
    total = 0
    for i in range(n):
        text = "alpha beta".upper().lower()
        total += len(text)
    return total


def split_loop(n):
    total = 0
    for i in range(n):
        parts = "native-pin-probe alpha".split()
        total += len(" ".join(parts))
    return total


def nested_loop(n):
    total = 0
    for outer in range(4):
        for i in range(n):
            text = "alpha".upper().lower()
            total += len(text)
    return total


def generated(n):
    for outer in range(3):
        total = 0
        for i in range(n):
            text = "alpha".upper().lower()
            total += len(text)
        yield total


def retain_pairs(n):
    items = ["alpha"] * n
    out = []
    for index, text in enumerate(items):
        out.append((index, text.upper().lower()))
    return out


class Formatted:
    def __init__(self, fail_at=0):
        self.calls = 0
        self.fail_at = fail_at

    def __str__(self):
        self.calls += 1
        if self.calls == self.fail_at:
            raise ValueError("format stop")
        return "alpha"


def format_loop(n, value):
    total = 0
    for i in range(n):
        text = "%s" % value
        total += len(text)
    return total


def invoke(n):
    return text_loop(n) + 1


was_enabled = gc.isenabled()
gc.disable()
try:
    for n in [0, 1, 3, 12, 12000, 20000]:
        assert text_loop(n) == n * 10
        assert split_loop(n) == n * 22
        assert nested_loop(n) == n * 20
        assert list(generated(n)) == [n * 5] * 3
        pairs = retain_pairs(n)
        assert pairs == [(index, "alpha") for index in range(n)]
        value = Formatted()
        assert format_loop(n, value) == n * 5
        assert value.calls == n
        del pairs, value
    # Warm one call site and wrapper to exercise native-to-native calls.
    for _ in range(12):
        assert invoke(12000) == 120001
    failing = Formatted(5001)
    try:
        format_loop(12000, failing)
    except ValueError as exc:
        assert str(exc) == "format stop"
    else:
        raise AssertionError("format callback did not raise")
    assert failing.calls == 5001
    assert not any(
        type(obj) is list and len(obj) == 2
        and type(obj[0]) is str and obj[0] == "native-pin-probe"
        for obj in gc.get_objects()
    )
finally:
    if was_enabled:
        gc.enable()
print("ok")
