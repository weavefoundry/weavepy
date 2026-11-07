"""Compiled reads of `datetime`, `date` and `timedelta` fields keep
CPython's results: every public field, a subclass that overrides `hour`
with its own property, one that adds an instance attribute, a missing
attribute, and values built by arithmetic and by parsing."""
from datetime import date, datetime, timedelta, timezone


class Shifted(datetime):
    @property
    def hour(self):
        return -1


class Tagged(datetime):
    pass


def fields(xs):
    out = []
    for x in xs:
        out.append((x.year, x.month, x.day, x.hour, x.minute, x.second, x.microsecond))
    return out


def tds(xs):
    return [(t.days, t.seconds, t.microseconds) for t in xs]


base = datetime(2024, 1, 15, 8, 30, 5, 7, tzinfo=timezone.utc)
step = timedelta(minutes=37, seconds=11, microseconds=3)
vals = [base + step * i for i in range(3000)]
want = [(v.year, v.month, v.day, v.hour, v.minute, v.second, v.microsecond) for v in vals]
for _ in range(3):
    assert fields(vals) == want
assert fields([Shifted(2020, 5, 6, 7, 8)]) == [(2020, 5, 6, -1, 8, 0, 0)]
t = Tagged(2021, 2, 3, 4, 5)
t.note = "x"
assert fields([t]) == [(2021, 2, 3, 4, 5, 0, 0)] and t.note == "x"
assert fields([datetime.fromisoformat(base.isoformat())]) == [want[0]]
assert tds([step * 3, -step, timedelta(days=2)]) == [(0, 6693, 9), (-1, 84168, 999997), (2, 0, 0)]
d = date(2024, 2, 29)
for _ in range(3000):
    assert (d.year, d.month, d.day) == (2024, 2, 29)
try:
    base.nope
except AttributeError:
    pass
else:
    raise AssertionError("no AttributeError")
print("ok")
