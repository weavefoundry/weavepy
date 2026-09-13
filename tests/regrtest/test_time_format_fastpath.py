"""Time formatting preserves precision, fallback callbacks, and unusual fields."""
import _pydatetime as pure
from datetime import datetime, time, timedelta, timezone

SPECS = ('hours', 'minutes', 'seconds', 'milliseconds', 'microseconds', 'auto')


def expected(hour, minute, second, micros, spec):
    text = '{:02d}:{:02d}:{:02d}.{:06d}'.format(hour, minute, second, micros)
    if spec == 'auto':
        return text[:8] if micros == 0 else text
    return text[:{'hours': 2, 'minutes': 5, 'seconds': 8,
                  'milliseconds': 12, 'microseconds': 15}[spec]]


def exercise_precisions():
    fields = [(hour, minute, second, micros)
              for hour, minute, second in [(0, 0, 0), (3, 4, 5), (9, 10, 11), (23, 59, 59)]
              for micros in [0, 1, 999, 1000, 1999, 9999, 10000, 99999, 100000, 999999]]
    seed = 740075
    for _ in range(256):
        seed = (seed * 1664525 + 1013904223) & 0xffffffff
        fields.append((seed % 24, (seed // 24) % 60, (seed // 1440) % 60,
                       (seed // 7) % 1000000))
    for hour, minute, second, micros in fields:
        for spec in SPECS:
            want = expected(hour, minute, second, micros, spec)
            assert pure._format_time(hour, minute, second, micros, spec) == want
            assert time(hour, minute, second, micros).isoformat(timespec=spec) == want
            assert datetime(2024, 2, 29, hour, minute, second, micros).isoformat(timespec=spec) == '2024-02-29T' + want
    for zone, suffix in [(timezone.utc, '+00:00'),
                         (timezone(timedelta(hours=5, minutes=30)), '+05:30'),
                         (timezone(-timedelta(hours=3, minutes=30)), '-03:30')]:
        for fold in [0, 1]:
            for spec in SPECS:
                want = expected(3, 4, 5, 1999, spec) + suffix
                assert time(3, 4, 5, 1999, tzinfo=zone, fold=fold).isoformat(timespec=spec) == want
                assert datetime(2024, 2, 29, 3, 4, 5, 1999, tzinfo=zone, fold=fold).isoformat(' ', timespec=spec) == '2024-02-29 ' + want


def exercise_fallback_fields():
    assert pure._format_time(-1, 2, 3, -1234, 'milliseconds') == '-1:02:03.-02'
    assert pure._format_time(124, 120, 73, 0, 'seconds') == '124:120:73'
    assert pure._format_time(10**40, 0, 0, 0, 'hours') == '1' + '0' * 40
    assert pure._format_time(True, False, True, True, 'microseconds') == '01:00:01.000001'
    assert pure._format_time(3, object(), object(), object(), 'hours') == '03'
    for spec in ['', 'Auto', 'nanoseconds', '秒', 'seconds\0']:
        try:
            pure._format_time(3, 4, 5, 1999, spec)
        except ValueError as error:
            assert str(error) == 'Unknown timespec value'
        else:
            raise AssertionError('invalid timespec accepted')


def exercise_callbacks():
    events = []

    class Field:
        def __init__(self, label):
            self.label = label

        def __format__(self, spec):
            events.append(('format', self.label, spec))
            return self.label + '<' + spec + '>'

        def __bool__(self):
            events.append(('bool', self.label))
            return True

        def __floordiv__(self, divisor):
            events.append(('floor', self.label, divisor))
            return Field(self.label + 'ms')

    hh, mm, ss, us = [Field(label) for label in ('H', 'M', 'S', 'U')]
    assert pure._format_time(hh, mm, ss, us, 'hours') == 'H<02d>'
    assert events == [('format', 'H', '02d')]
    events.clear()
    assert pure._format_time(hh, mm, ss, us, 'auto') == 'H<02d>:M<02d>:S<02d>.U<06d>'
    assert events == [('bool', 'U'), ('format', 'H', '02d'), ('format', 'M', '02d'),
                      ('format', 'S', '02d'), ('format', 'U', '06d')]
    events.clear()
    assert pure._format_time(hh, mm, ss, us, 'milliseconds') == 'H<02d>:M<02d>:S<02d>.Ums<03d>'
    assert events == [('floor', 'U', 1000), ('format', 'H', '02d'), ('format', 'M', '02d'),
                      ('format', 'S', '02d'), ('format', 'Ums', '03d')]

    class Integer(int):
        def __format__(self, spec):
            events.append(('integer', int(self), spec))
            return super().__format__(spec)

    events.clear()
    assert pure._format_time(Integer(3), 4, 5, 0, 'seconds') == '03:04:05'
    assert events == [('integer', 3, '02d')]

    class Spec(str):
        pass

    assert pure._format_time(3, 4, 5, 1999, Spec('milliseconds')) == '03:04:05.001'


def exercise_helper_fallback_and_rebinding():
    native = getattr(pure, '_format_time_parts', None)
    if native is None:
        return
    assert native(3, 4, 5, 1999, 'milliseconds') == '03:04:05.001'
    assert native(True, 4, 5, 1999, 'milliseconds') is None
    assert native(3, 4, 5, -1, 'auto') is None
    assert native(3, 4, 5, 0, 'nanoseconds') is None
    try:
        pure._format_time_parts = None
        for _ in range(60):
            assert pure._format_time(3, 4, 5, 1999, 'milliseconds') == '03:04:05.001'
    finally:
        pure._format_time_parts = native
    assert pure._format_time(3, 4, 5, 1999, 'milliseconds') == '03:04:05.001'


exercise_precisions()
exercise_fallback_fields()
exercise_callbacks()
exercise_helper_fallback_and_rebinding()
print('time formatting semantics: ok')
