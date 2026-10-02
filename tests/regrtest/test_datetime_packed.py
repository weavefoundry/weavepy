"""Natively built datetime values (packed storage) behave like Python-built ones."""
import copy
import gc
import pickle
from datetime import date, datetime, time, timedelta, timezone, tzinfo


class SubDate(date):
    pass


class SubDateTime(datetime):
    pass


class SubDelta(timedelta):
    pass


utc = timezone.utc
east = timezone(timedelta(hours=5, minutes=30))
odd = timezone(timedelta(hours=-3, seconds=7, microseconds=11))

# Slot reads from Python methods see the packed fields.
d = datetime(2024, 2, 29, 23, 59, 58, 999999, tzinfo=east)
# The pure-Python classes' private slots (CPython's C types have none).
PURE = hasattr(d, "_year")
if PURE:
    assert (d._year, d._month, d._day, d._hour, d._minute) == (2024, 2, 29, 23, 59)
    assert (d._second, d._microsecond, d._tzinfo, d._fold) == (58, 999999, east, 0)
assert repr(d) == ("datetime.datetime(2024, 2, 29, 23, 59, 58, 999999, "
                   "tzinfo=datetime.timezone(datetime.timedelta(seconds=19800)))")
assert str(d) == "2024-02-29 23:59:58.999999+05:30"
assert d.utcoffset() == timedelta(hours=5, minutes=30)
assert d.timetuple()[:6] == (2024, 2, 29, 23, 59, 58)
assert d.astimezone(utc) == d and d.astimezone(utc).hour == 18
t = timedelta(days=-3, seconds=5, microseconds=7)
assert (t.days, t.seconds, t.microseconds) == (-3, 5, 7)
if PURE:
    assert (t._days, t._seconds, t._microseconds) == (-3, 5, 7)
assert repr(t) == "datetime.timedelta(days=-3, seconds=5, microseconds=7)"
assert t.total_seconds() == -259194.999993

# Pickling and copying round-trip, through every protocol.
for value in [d, t, date(1, 1, 1), datetime(9999, 12, 31, 23, 59, 59, 999999),
              datetime(2000, 1, 1, fold=1), datetime(2000, 1, 1, tzinfo=odd)]:
    for proto in range(pickle.HIGHEST_PROTOCOL + 1):
        back = pickle.loads(pickle.dumps(value, proto))
        assert back == value and type(back) is type(value)
    assert copy.copy(value) == value and copy.deepcopy(value) == value
assert pickle.loads(pickle.dumps(datetime(2000, 1, 1, fold=1), 4)).fold == 1

# A written slot keeps the other fields.
w = datetime(2021, 3, 4, 5, 6, 7)
if PURE:
    w._hashcode = 12345
    assert w._hashcode == 12345 and w == datetime(2021, 3, 4, 5, 6, 7)
assert (w.year, w.month, w.day, w.hour, w.minute, w.second) == (2021, 3, 4, 5, 6, 7)

# Native hashes equal the Python implementation's (which subclasses run).
for a, b in [(date(2024, 2, 29), SubDate(2024, 2, 29)),
             (datetime(2024, 2, 29, 1, 2, 3, 456789),
              SubDateTime(2024, 2, 29, 1, 2, 3, 456789)),
             (datetime(2024, 2, 29, 1, 2, 3, tzinfo=odd),
              SubDateTime(2024, 2, 29, 1, 2, 3, tzinfo=odd)),
             (datetime(1, 1, 1, tzinfo=east), SubDateTime(1, 1, 1, tzinfo=east)),
             (datetime(2000, 1, 1, fold=1), SubDateTime(2000, 1, 1, fold=1)),
             (timedelta(1, 2, 3), SubDelta(1, 2, 3)),
             (timedelta(-999999999), SubDelta(-999999999))]:
    assert a == b and hash(a) == hash(b), (a, b)
assert hash(datetime(2020, 1, 1, 12, tzinfo=utc)) == hash(
    datetime(2020, 1, 1, 17, 30, tzinfo=east))

# The collector sees the tzinfo (CPython's C type is not a GC type).
assert not PURE or east in gc.get_referents(d)

# Arithmetic within and across days, both directions.
base = datetime(2024, 1, 15, 8, 30, tzinfo=utc)
step = timedelta(minutes=37, seconds=11)
cur = base
for i in range(200):
    cur = cur + step
assert cur == datetime(2024, 1, 20, 12, 26, 40, tzinfo=utc)
assert cur - base == step * 200 and base - cur == -(step * 200)
assert (cur - step * 200) == base
assert datetime(2024, 1, 1) - timedelta(microseconds=1) == datetime(2023, 12, 31, 23, 59, 59, 999999)
assert datetime(2000, 3, 1, fold=1) + timedelta(0) == datetime(2000, 3, 1)
assert (datetime(2000, 3, 1, fold=1) + timedelta(0)).fold == 0
assert datetime(2024, 1, 1, 12, tzinfo=utc) - datetime(2024, 1, 1, 12, tzinfo=east) == timedelta(hours=5, minutes=30)
for big in [timedelta(days=4000000), timedelta(days=-800000)]:
    try:
        datetime(2000, 1, 1) + big
    except OverflowError:
        pass
    else:
        raise AssertionError("out-of-range datetime accepted")
try:
    timedelta(days=999999999) + timedelta(days=1)
except OverflowError:
    pass
else:
    raise AssertionError("out-of-range timedelta accepted")

# Comparisons: fixed offsets, naive and aware, mixed kinds.
assert datetime(2024, 1, 1, 12, tzinfo=utc) == datetime(2024, 1, 1, 17, 30, tzinfo=east)
assert datetime(2024, 1, 1, 12, tzinfo=utc) < datetime(2024, 1, 1, 18, tzinfo=east)
assert datetime(2024, 1, 1) != datetime(2024, 1, 1, tzinfo=utc)
try:
    datetime(2024, 1, 1) < datetime(2024, 1, 1, tzinfo=utc)
except TypeError:
    pass
else:
    raise AssertionError("naive/aware ordering accepted")
assert datetime(2000, 1, 1, fold=1) == datetime(2000, 1, 1)
assert timedelta(-1) < timedelta(0) < timedelta(microseconds=1) < timedelta(seconds=1)
assert date(2024, 1, 1) != datetime(2024, 1, 1)

# Constructors: keywords, defaults, fallbacks and errors.
assert timedelta(weeks=1, days=-1, hours=1, minutes=2, seconds=3,
                 milliseconds=4, microseconds=5) == timedelta(6, 3723, 4005)
assert timedelta(hours=1.5) == timedelta(seconds=5400)
assert timedelta(True) == timedelta(1)
for bad in [dict(days=10**9), dict(days=2**70)]:
    try:
        timedelta(**bad)
    except OverflowError:
        pass
    else:
        raise AssertionError("out-of-range timedelta accepted")
assert date(year=2020, day=2, month=1) == date(2020, 1, 2)
assert datetime(2020, 5, 3, hour=4, tzinfo=utc) == datetime(2020, 5, 3, 4, 0, 0, 0, utc)
assert datetime(2020, 1, 1, fold=1).fold == 1
for args, kwargs, exc in [((2020, 2, 30), {}, ValueError), ((2020, 1, 1, 24), {}, ValueError),
                          ((2020, 1, 1), {"fold": 2}, ValueError),
                          ((2020, 1, 1), {"year": 2020}, TypeError),
                          ((2020, 1, 1), {"bogus": 1}, TypeError),
                          ((2020, 1, 1, 0, 0, 0, 0, None, 0), {}, TypeError)]:
    try:
        datetime(*args, **kwargs)
    except exc:
        pass
    else:
        raise AssertionError((args, kwargs))
assert type(SubDateTime(2020, 1, 1)) is SubDateTime
assert type(SubDelta(hours=1)) is SubDelta

# replace(), positional and keyword.
x = date(2024, 2, 29)
assert x.replace(day=1) == date(2024, 2, 1)
assert x.replace(2023, 3) == date(2023, 3, 29)
assert x.replace(year=2023, month=3, day=31) == date(2023, 3, 31)
try:
    x.replace(year=2023)
except ValueError:
    pass
else:
    raise AssertionError("2023-02-29 accepted")
assert type(SubDate(2024, 1, 1).replace(day=2)) is SubDate

# fromisoformat: bound to the class it is read from.
m = datetime.fromisoformat
assert m.__self__ is datetime and m == datetime.fromisoformat
assert datetime.fromisoformat("2024-01-15T08:30:00+00:00") == base
assert datetime.fromisoformat("2024-01-15T08:30:00+00:00").tzinfo is utc
assert datetime.fromisoformat("2024-02-29T23:59:58.999999+05:30") == d
assert type(SubDateTime.fromisoformat("2024-01-15T08:30:00")) is SubDateTime
assert type(base.fromisoformat("2024-01-15")) is datetime

# Formatting.
assert d.isoformat() == "2024-02-29T23:59:58.999999+05:30"
assert d.isoformat(" ", "minutes") == "2024-02-29 23:59+05:30"
assert d.isoformat("é", "milliseconds") == "2024-02-29é23:59:58.999+05:30"
assert datetime(2020, 1, 1, tzinfo=odd).isoformat() == "2020-01-01T00:00:00-02:59:52.999989"
assert datetime(5, 1, 2, 3, 4, 5).isoformat() == "0005-01-02T03:04:05"
assert d.strftime("%Y-%m-%d %H:%M:%S.%f %z|%:z %j %y %I%%") == \
    "2024-02-29 23:59:58.999999 +0530|+05:30 060 24 11%"
assert date(2024, 2, 29).isoformat() == "2024-02-29"
assert date(2024, 2, 29).strftime("%F é %T") == "2024-02-29 é 00:00:00"

# A tzinfo held only by a native result is released (and finalized) with it.
deaths = []


class Zone(tzinfo):
    def utcoffset(self, dt):
        return timedelta(hours=1)

    def dst(self, dt):
        return timedelta(0)

    def __del__(self):
        deaths.append(1)


v = datetime(2020, 1, 1, tzinfo=Zone()) + timedelta(hours=1)
assert v.utcoffset() == timedelta(hours=1)
del v
gc.collect()
assert deaths == [1], deaths

# Values outlive their pooled neighbours.
keep = [datetime(2000, 1, 1) + timedelta(days=i) for i in range(300)]
for i in range(1000):
    datetime(2000, 1, 1) + timedelta(seconds=i)
assert keep[-1] == datetime(2000, 10, 26) and len(set(keep)) == 300

print("datetime packed values: ok")
