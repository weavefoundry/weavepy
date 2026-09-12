"""Native ISO time components retain the Python parser's fallback semantics."""
import datetime
import _pydatetime as reference

try:
    from _weave_datetime import parse_time_parts
except ImportError:
    parse_time_parts = None

cases = [
    ('00', [0, 0, 0, 0]),
    ('1234', [12, 34, 0, 0]),
    ('12:34', [12, 34, 0, 0]),
    ('123456', [12, 34, 56, 0]),
    ('12:34:56', [12, 34, 56, 0]),
    ('12:34:56.123456', [12, 34, 56, 123456]),
    ('123456,000001999', [12, 34, 56, 1]),
    ('99:99:99', [99, 99, 99, 0]),
]
for text, expected in cases:
    assert reference._parse_hh_mm_ss_ff(text) == expected
    if parse_time_parts is not None:
        first = parse_time_parts(text)
        second = parse_time_parts(text)
        assert type(first) is list and first == expected
        assert first is not second
        first.append(17)
        assert second == expected

if parse_time_parts is not None:
    for text in ['', '1', '12:3', '12:3456', '1234:56', '12:34:56Z',
                 '12:34:56.1', '12:34:56.12345', '12:34:56.123456x',
                 '１２:３４:５６', '12:34:56.١٢٣٤٥٦']:
        assert parse_time_parts(text) is None
    assert parse_time_parts(123456) is None

# A str subclass retains its Python indexing and length callbacks.
events = []
class Text(str):
    def __len__(self):
        events.append('len')
        return super().__len__()
    def __getitem__(self, key):
        events.append(('get', repr(key)))
        return super().__getitem__(key)

text = Text('12:34:56.123456')
if parse_time_parts is not None:
    assert parse_time_parts(text) is None and events == []
assert reference._parse_hh_mm_ss_ff(text) == [12, 34, 56, 123456]
assert events and events[0] == 'len'
if hasattr(reference, '_parse_time_parts'):
    native = reference._parse_time_parts
    with_native = list(events)
    try:
        reference._parse_time_parts = None
        events.clear()
        assert reference._parse_hh_mm_ss_ff(text) == [12, 34, 56, 123456]
        assert events == with_native
    finally:
        reference._parse_time_parts = native

# Short fractions continue to use the reference correction table.
saved = reference._FRACTION_CORRECTION[0]
try:
    reference._FRACTION_CORRECTION[0] = 37
    assert reference._parse_hh_mm_ss_ff('00:00:00.1') == [0, 0, 0, 37]
finally:
    reference._FRACTION_CORRECTION[0] = saved

class Moment(datetime.datetime):
    pass
class Clock(datetime.time):
    pass

for cls, text in [(datetime.datetime, '2024-02-29T12:34:56.123456+01:30'),
                  (Moment, '2024-02-29T12:34:56.123456+01:30'),
                  (datetime.time, '12:34:56.123456+01:30'),
                  (Clock, '12:34:56.123456+01:30')]:
    value = cls.fromisoformat(text)
    assert type(value) is cls
    assert (value.hour, value.minute, value.second, value.microsecond) == (12, 34, 56, 123456)
    assert value.utcoffset() == datetime.timedelta(minutes=90)

print('datetime time parts: ok')
