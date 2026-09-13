"""Run string count callbacks once with current globals and caller frames."""

import sys

OFFSET = 1


class Limit:
    def __init__(self, mutate=False, fail_at=0):
        self.calls = 0
        self.callers = set()
        self.mutate = mutate
        self.fail_at = fail_at

    def __index__(self):
        global OFFSET
        self.calls += 1
        self.callers.add(sys._getframe(1).f_code.co_name)
        if self.mutate:
            OFFSET += 1
        if self.calls == self.fail_at:
            raise ValueError("count callback stop")
        return 1


def replace_loop(limit, n):
    total = 0
    for i in range(n):
        text = "alpha".replace("a", "b", limit)
        total += OFFSET
    return total


def replace_once(limit):
    return len("alpha".replace("a", "b", limit))


def invoke(limit):
    return replace_once(limit)


for _ in range(12):
    limit = Limit(mutate=True)
    OFFSET = 1
    assert replace_loop(limit, 3000) == 4504500
    assert limit.calls == 3000
    assert limit.callers == {"replace_loop"}
    assert OFFSET == 3001

limit = Limit()
total = 0
for _ in range(300):
    total += invoke(limit)
assert total == 1500
assert limit.calls == 300
assert limit.callers == {"replace_once"}

limit = Limit(fail_at=79)
try:
    for _ in range(300):
        invoke(limit)
except ValueError as exc:
    assert str(exc) == "count callback stop"
else:
    raise AssertionError("count callback did not raise")
assert limit.calls == 79
assert limit.callers == {"replace_once"}
print("ok")
