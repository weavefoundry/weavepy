"""`datetime` methods that are written in Python over natively stored
values (time zone conversion, `replace`, `timestamp`, tuples, pickling,
hashing) give the same answers in hot code as on a first call, for plain
values, values with a time zone and a fold, and subclasses."""

import copy
import datetime as dt
import pickle
import time

N = 600
UTC = dt.timezone.utc
EAST = dt.timezone(dt.timedelta(hours=5, minutes=30), "EAST")


class Stamp(dt.datetime):
    def label(self):
        return "%04d/%02d" % (self.year, self.month)


class Span(dt.timedelta):
    pass


def conversions(n):
    naive = dt.datetime(2024, 5, 17, 12, 30, 15, 250000)
    aware = naive.replace(tzinfo=EAST)
    for i in range(n):
        d = naive + dt.timedelta(seconds=i)
        local = d.astimezone()
        assert local.tzinfo is not None
        assert local.replace(tzinfo=None) == d
        assert d.astimezone(UTC).timestamp() == d.timestamp()
        assert local.utcoffset() == dt.timedelta(seconds=time.localtime(d.timestamp()).tm_gmtoff)
        a = aware + dt.timedelta(minutes=i)
        u = a.astimezone(UTC)
        assert u == a and u.utcoffset() == dt.timedelta(0)
        assert u.replace(tzinfo=None) == a.replace(tzinfo=None) - dt.timedelta(hours=5, minutes=30)
        assert a.astimezone(EAST) is a
        assert a.timestamp() == u.timestamp()
        assert a.utctimetuple()[:6] == u.timetuple()[:6]
    return True


def fields(n):
    d = dt.datetime(2023, 11, 5, 1, 30, 45, 123456, tzinfo=EAST, fold=1)
    td = dt.timedelta(days=-3, seconds=4000, microseconds=77)
    day = dt.date(1999, 12, 31)
    for _ in range(n):
        assert d.replace(year=2024) == dt.datetime(2024, 11, 5, 1, 30, 45, 123456, tzinfo=EAST)
        assert d.replace(year=2024).fold == 1 and d.replace(fold=0).fold == 0
        assert d.replace(tzinfo=None).tzinfo is None and d.replace(hour=7).hour == 7
        assert d.timetuple()[:7] == (2023, 11, 5, 1, 30, 45, 6)
        assert d.isoformat() == "2023-11-05T01:30:45.123456+05:30"
        assert d.time() == dt.time(1, 30, 45, 123456, fold=1) and d.timetz().tzinfo is EAST
        assert d.date() == dt.date(2023, 11, 5) and d.toordinal() == 738829
        assert d.tzname() == "EAST" and d.dst() is None
        assert hash(d) == hash(d.astimezone(UTC).replace(fold=0))
        assert (td.days, td.seconds, td.microseconds) == (-3, 4000, 77)
        assert td.total_seconds() == -3 * 86400 + 4000 + 77e-6
        assert abs(td) == dt.timedelta(days=2, seconds=82399, microseconds=999923)
        assert str(td) == "-3 days, 1:06:40.000077" and repr(td).startswith("datetime.timedelta(")
        assert day.replace(day=1) == dt.date(1999, 12, 1) and day.isocalendar()[:2] == (1999, 52)
        assert day.timetuple()[:3] == (1999, 12, 31) and day.ctime() == "Fri Dec 31 00:00:00 1999"
    return True


def copies(n):
    values = [
        dt.datetime(2024, 5, 17, 12, 30, 15),
        dt.datetime(2024, 5, 17, 12, 30, 15, 99, tzinfo=UTC, fold=1),
        dt.date(2024, 2, 29),
        dt.timedelta(days=1, seconds=2, microseconds=3),
        dt.time(23, 59, 58, 7, tzinfo=EAST),
        Stamp(2001, 2, 3, 4, 5, 6),
        Span(days=7),
    ]
    for _ in range(n):
        for v in values:
            for back in (pickle.loads(pickle.dumps(v)), copy.copy(v), copy.deepcopy(v)):
                assert back == v and type(back) is type(v) and hash(back) == hash(v)
                assert repr(back) == repr(v)
        assert values[1].__reduce__()[0] is dt.datetime and getattr(values[1], "fold") == 1
    return True


def subclasses(n):
    s = Stamp(2024, 5, 17, 12, 30, 15, tzinfo=UTC)
    span = Span(hours=36)
    for _ in range(n):
        assert s.label() == "2024/05" and s.astimezone(EAST).hour == 18
        assert type(s.replace(year=2000)) is Stamp and s.replace(year=2000).label() == "2000/05"
        assert type(s + span) is dt.datetime or type(s + span) is Stamp
        assert (s + span).day == 19 and (s - span).day == 16
        assert span.days == 1 and span.seconds == 43200 and span.total_seconds() == 129600.0
        assert s.timestamp() == 1715949015.0 and s.utcoffset() == dt.timedelta(0)
    return True


for _ in range(3):
    assert conversions(N) and fields(N) and copies(N // 6) and subclasses(N)

print("ok")
