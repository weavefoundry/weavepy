"""Ecosystem probe: python-dateutil — parser, relativedelta, rrule, tz."""

import datetime

import dateutil
from dateutil import parser, rrule, tz
from dateutil.relativedelta import relativedelta

dt = parser.parse("2026-07-19T12:34:56")
assert dt == datetime.datetime(2026, 7, 19, 12, 34, 56)

assert parser.parse("July 4, 1976").date() == datetime.date(1976, 7, 4)

# relativedelta
nxt = datetime.date(2026, 1, 31) + relativedelta(months=1)
assert nxt == datetime.date(2026, 2, 28), nxt

# rrule expansion
rule = rrule.rrule(
    rrule.WEEKLY,
    dtstart=datetime.datetime(2026, 1, 5),
    count=3,
)
got = [d.date() for d in rule]
assert got == [
    datetime.date(2026, 1, 5),
    datetime.date(2026, 1, 12),
    datetime.date(2026, 1, 19),
], got

# tz
utc = tz.UTC
aware = datetime.datetime(2026, 7, 19, tzinfo=utc)
assert aware.utcoffset() == datetime.timedelta(0)

print("dateutil ok", dateutil.__version__)
