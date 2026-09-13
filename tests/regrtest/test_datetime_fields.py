"""Fast field validation preserves conversions, fallback errors, and subclasses."""
import datetime
import _pydatetime as reference

try:
    from _weave_datetime import date_fields, time_fields
except ImportError:
    date_fields = time_fields = None

if date_fields is not None:
    assert date_fields(2000, 2, 29) == (2000, 2, 29)
    assert type(date_fields(9999, 12, 31)) is tuple
    assert time_fields(23, 59, 59, 999999, 1) == (23, 59, 59, 999999, 1)
    for parts in [(1900, 2, 29), (0, 1, 1), (10000, 1, 1),
                  (2024, 0, 1), (2024, 13, 1), (2024, 4, 31),
                  (2024, 1, 0), (True, 1, 1), (2024.0, 1, 1),
                  (2**100, 1, 1), (2024, 1), (2024, 1, 1, 1)]:
        assert date_fields(*parts) is None
    for parts in [(24, 0, 0, 0, 0), (0, 60, 0, 0, 0), (0, 0, 60, 0, 0),
                  (0, 0, 0, 1000000, 0), (0, 0, 0, 0, 2),
                  (0, 0, 0, 0, True), (0.0, 0, 0, 0, 0),
                  (0, 0, 0, 0), (0, 0, 0, 0, 0, 0)]:
        assert time_fields(*parts) is None

events = []
class Index:
    def __init__(self, name, value):
        self.name, self.value = name, value
    def __index__(self):
        events.append(self.name)
        return self.value

date_args = [Index('year', 2024), Index('month', 2), Index('day', 29)]
time_args = [Index('hour', 12), Index('minute', 34),
             Index('second', 56), Index('microsecond', 123456), 0]
if date_fields is not None:
    assert date_fields(*date_args) is None
    assert time_fields(*time_args) is None
    assert events == []
assert reference._check_date_fields(*date_args) == (2024, 2, 29)
assert events == ['year', 'month', 'day']
events.clear()
assert reference._check_time_fields(*time_args) == (12, 34, 56, 123456, 0)
assert events == ['hour', 'minute', 'second', 'microsecond']

# All conversions precede validation, even when the first field is invalid.
events.clear()
try:
    reference._check_date_fields(0, Index('month', 2), Index('day', 29))
except ValueError:
    pass
else:
    raise AssertionError('invalid year accepted')
assert events == ['month', 'day']

# Fold is retained as supplied by the existing Python validator.
assert reference._check_time_fields(0, 0, 0, 0, True)[4] is True

class Day(datetime.date):
    pass
class Moment(datetime.datetime):
    pass
class Clock(datetime.time):
    pass

for cls, args, kw in [(Day, (2024, 2, 29), {}),
                      (Moment, (2024, 2, 29, 12, 34, 56, 123456),
                       {'fold': 1, 'tzinfo': datetime.timezone.utc}),
                      (Clock, (12, 34, 56, 123456), {'fold': 1})]:
    value = cls(*args, **kw)
    assert type(value) is cls
    assert value.replace() == value
    if cls is not Day:
        assert value.fold == 1

# Verify the public paths reach both native helpers after warmup.
if date_fields is not None:
    for _ in range(2000):
        datetime.datetime(2024, 2, 29, 12, 34, 56, 123456)
    saved_date, saved_time = reference._date_fields, reference._time_fields
    calls = []
    def record_date(*args):
        result = saved_date(*args)
        calls.append(('date', result is not None))
        return result
    def record_time(*args):
        result = saved_time(*args)
        calls.append(('time', result is not None))
        return result
    try:
        reference._date_fields, reference._time_fields = record_date, record_time
        datetime.datetime(2024, 2, 29, 12, 34, 56, 123456)
        assert calls == [('date', True), ('time', True)]
        calls.clear()
        datetime.date(Index('year', 2024), 2, 29)
        assert calls == [('date', False)]
    finally:
        reference._date_fields, reference._time_fields = saved_date, saved_time

print('datetime fields: ok')
