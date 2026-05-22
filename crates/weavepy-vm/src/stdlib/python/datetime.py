"""WeavePy `datetime` — pure-Python wrapper over `_datetime`.

Mirrors the CPython 3.13 surface for typical user code. Calendar
arithmetic is delegated to the Rust `_datetime` module for accuracy
on epoch / Gregorian conversions; everything else is in Python.
"""

import _datetime as _impl


MINYEAR = _impl.MINYEAR
MAXYEAR = _impl.MAXYEAR


__all__ = [
    "MINYEAR",
    "MAXYEAR",
    "date",
    "time",
    "datetime",
    "timedelta",
    "timezone",
    "tzinfo",
    "UTC",
]


def _check_int(value, name):
    if isinstance(value, bool):
        return int(value)
    if not isinstance(value, int):
        raise TypeError(f"{name} must be int, not {type(value).__name__}")
    return value


def _divmod(a, b):
    q = a // b
    r = a - q * b
    return q, r


# ----------- timedelta ----------- #

class timedelta:
    """A duration of time. Stored as (days, seconds, microseconds)."""

    __slots__ = ("_days", "_seconds", "_microseconds", "_hashcode")

    min = None
    max = None
    resolution = None

    def __new__(cls, days=0, seconds=0, microseconds=0, milliseconds=0,
                minutes=0, hours=0, weeks=0):
        total_us = (
            ((weeks * 7 + days) * 24 * 3600 + hours * 3600 + minutes * 60 + seconds) * 1_000_000
            + milliseconds * 1000
            + microseconds
        )
        total_us = int(total_us)
        d, rem = _divmod(total_us, 86_400 * 1_000_000)
        s, us = _divmod(rem, 1_000_000)
        self = object.__new__(cls)
        self._days = d
        self._seconds = s
        self._microseconds = us
        self._hashcode = -1
        return self

    @property
    def days(self):
        return self._days

    @property
    def seconds(self):
        return self._seconds

    @property
    def microseconds(self):
        return self._microseconds

    def total_seconds(self):
        return ((self._days * 86_400 + self._seconds) * 1_000_000 + self._microseconds) / 1_000_000

    def __repr__(self):
        parts = []
        if self._days:
            parts.append(f"days={self._days}")
        if self._seconds:
            parts.append(f"seconds={self._seconds}")
        if self._microseconds:
            parts.append(f"microseconds={self._microseconds}")
        return "datetime.timedelta(" + (", ".join(parts) if parts else "0") + ")"

    def __str__(self):
        mm, ss = _divmod(self._seconds, 60)
        hh, mm = _divmod(mm, 60)
        s = f"{hh}:{mm:02d}:{ss:02d}"
        if self._days:
            plural = "s" if abs(self._days) != 1 else ""
            s = f"{self._days} day{plural}, " + s
        if self._microseconds:
            s += f".{self._microseconds:06d}"
        return s

    def __bool__(self):
        return self._days != 0 or self._seconds != 0 or self._microseconds != 0

    def __add__(self, other):
        if isinstance(other, timedelta):
            return timedelta(
                days=self._days + other._days,
                seconds=self._seconds + other._seconds,
                microseconds=self._microseconds + other._microseconds,
            )
        return NotImplemented

    def __radd__(self, other):
        return self.__add__(other)

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return timedelta(
                days=self._days - other._days,
                seconds=self._seconds - other._seconds,
                microseconds=self._microseconds - other._microseconds,
            )
        return NotImplemented

    def __rsub__(self, other):
        if isinstance(other, timedelta):
            return -self + other
        return NotImplemented

    def __neg__(self):
        return timedelta(
            days=-self._days,
            seconds=-self._seconds,
            microseconds=-self._microseconds,
        )

    def __pos__(self):
        return self

    def __abs__(self):
        if self._days < 0:
            return -self
        return self

    def __mul__(self, other):
        if isinstance(other, int):
            return timedelta(
                days=self._days * other,
                seconds=self._seconds * other,
                microseconds=self._microseconds * other,
            )
        if isinstance(other, float):
            total = self.total_seconds() * other
            return timedelta(seconds=total)
        return NotImplemented

    __rmul__ = __mul__

    def __floordiv__(self, other):
        if isinstance(other, int):
            us = (self._days * 86_400 + self._seconds) * 1_000_000 + self._microseconds
            return timedelta(microseconds=us // other)
        if isinstance(other, timedelta):
            a = (self._days * 86_400 + self._seconds) * 1_000_000 + self._microseconds
            b = (other._days * 86_400 + other._seconds) * 1_000_000 + other._microseconds
            return a // b
        return NotImplemented

    def __truediv__(self, other):
        if isinstance(other, (int, float)):
            return timedelta(seconds=self.total_seconds() / other)
        if isinstance(other, timedelta):
            return self.total_seconds() / other.total_seconds()
        return NotImplemented

    def __mod__(self, other):
        if isinstance(other, timedelta):
            a = (self._days * 86_400 + self._seconds) * 1_000_000 + self._microseconds
            b = (other._days * 86_400 + other._seconds) * 1_000_000 + other._microseconds
            return timedelta(microseconds=a % b)
        return NotImplemented

    def __divmod__(self, other):
        if isinstance(other, timedelta):
            return self // other, self % other
        return NotImplemented

    def _cmp_key(self):
        return (self._days, self._seconds, self._microseconds)

    def __eq__(self, other):
        if isinstance(other, timedelta):
            return self._cmp_key() == other._cmp_key()
        return NotImplemented

    def __ne__(self, other):
        eq = self.__eq__(other)
        if eq is NotImplemented:
            return eq
        return not eq

    def __lt__(self, other):
        if isinstance(other, timedelta):
            return self._cmp_key() < other._cmp_key()
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, timedelta):
            return self._cmp_key() <= other._cmp_key()
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, timedelta):
            return self._cmp_key() > other._cmp_key()
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, timedelta):
            return self._cmp_key() >= other._cmp_key()
        return NotImplemented

    def __hash__(self):
        return hash(self._cmp_key())


timedelta.min = timedelta(-999_999_999)
timedelta.max = timedelta(days=999_999_999, hours=23, minutes=59, seconds=59,
                          microseconds=999_999)
timedelta.resolution = timedelta(microseconds=1)


# ----------- tzinfo / timezone ----------- #

class tzinfo:
    """Abstract base class for time zones."""

    def utcoffset(self, dt):
        raise NotImplementedError

    def dst(self, dt):
        raise NotImplementedError

    def tzname(self, dt):
        raise NotImplementedError

    def fromutc(self, dt):
        offset = self.utcoffset(dt)
        if offset is None:
            raise ValueError("fromutc() requires a non-None utcoffset()")
        dst = self.dst(dt)
        if dst is None:
            raise ValueError("fromutc() requires a non-None dst()")
        return dt + offset


class timezone(tzinfo):
    """Fixed-offset timezone."""

    __slots__ = ("_offset", "_name")

    utc = None

    def __init__(self, offset, name=None):
        if not isinstance(offset, timedelta):
            raise TypeError("offset must be a timedelta")
        self._offset = offset
        self._name = name

    def utcoffset(self, dt):
        return self._offset

    def dst(self, dt):
        return timedelta(0)

    def tzname(self, dt):
        if self._name is not None:
            return self._name
        total = int(self._offset.total_seconds())
        if total == 0:
            return "UTC"
        sign = "+" if total >= 0 else "-"
        total = abs(total)
        hours, rem = _divmod(total, 3600)
        minutes, _ = _divmod(rem, 60)
        return f"UTC{sign}{hours:02d}:{minutes:02d}"

    def __repr__(self):
        if self._name is not None:
            return f"datetime.timezone(timedelta(seconds={int(self._offset.total_seconds())}), {self._name!r})"
        return f"datetime.timezone(timedelta(seconds={int(self._offset.total_seconds())}))"

    def __eq__(self, other):
        if isinstance(other, timezone):
            return self._offset == other._offset
        return NotImplemented

    def __hash__(self):
        return hash(self._offset)


timezone.utc = timezone(timedelta(0), "UTC")
timezone.min = timezone(timedelta(hours=-23, minutes=-59))
timezone.max = timezone(timedelta(hours=23, minutes=59))


UTC = timezone.utc


# ----------- date ----------- #

class date:
    """A naive calendar date."""

    __slots__ = ("_year", "_month", "_day", "_hashcode")

    min = None
    max = None
    resolution = timedelta(days=1)

    def __new__(cls, year, month, day):
        year = _check_int(year, "year")
        month = _check_int(month, "month")
        day = _check_int(day, "day")
        if not (MINYEAR <= year <= MAXYEAR):
            raise ValueError(f"year {year} out of range")
        if not (1 <= month <= 12):
            raise ValueError("month must be in 1..12")
        dim = _impl.days_in_month(year, month)
        if not (1 <= day <= dim):
            raise ValueError(f"day must be in 1..{dim}")
        self = object.__new__(cls)
        self._year = year
        self._month = month
        self._day = day
        self._hashcode = -1
        return self

    @classmethod
    def today(cls):
        y, mo, d, _, _, _, _, _ = _impl.now_components()
        return cls(y, mo, d)

    @classmethod
    def fromtimestamp(cls, t):
        y, mo, d, _, _, _, _, _ = _impl.from_timestamp(t, False)
        return cls(y, mo, d)

    @classmethod
    def fromordinal(cls, n):
        y, mo, d = _impl.ordinal_to_components(n)
        return cls(y, mo, d)

    @classmethod
    def fromisoformat(cls, s):
        if len(s) != 10 or s[4] != "-" or s[7] != "-":
            raise ValueError(f"Invalid isoformat string: {s!r}")
        return cls(int(s[0:4]), int(s[5:7]), int(s[8:10]))

    @classmethod
    def fromisocalendar(cls, year, week, day):
        # Approximate inverse of isocalendar; iterate days until a
        # match. Acceptable for non-hot paths.
        if not (1 <= day <= 7):
            raise ValueError("day must be in 1..7")
        start = cls(year, 1, 4)  # Always in ISO week 1.
        ordinal = start.toordinal()
        _, _, sd = _impl.iso_calendar(year, 1, 4)
        target_ordinal = ordinal - (sd - 1) + (week - 1) * 7 + (day - 1)
        return cls.fromordinal(target_ordinal)

    @property
    def year(self):
        return self._year

    @property
    def month(self):
        return self._month

    @property
    def day(self):
        return self._day

    def toordinal(self):
        return _impl.days_to_ordinal(self._year, self._month, self._day)

    def weekday(self):
        return _impl.weekday(self._year, self._month, self._day)

    def isoweekday(self):
        return self.weekday() + 1

    def isocalendar(self):
        return _impl.iso_calendar(self._year, self._month, self._day)

    def isoformat(self):
        return f"{self._year:04d}-{self._month:02d}-{self._day:02d}"

    __str__ = isoformat

    def __repr__(self):
        return f"datetime.date({self._year}, {self._month}, {self._day})"

    def ctime(self):
        return self._strftime("%a %b %e %H:%M:%S %Y")

    def strftime(self, fmt):
        return self._strftime(fmt)

    def _strftime(self, fmt):
        return _strftime(fmt, self._year, self._month, self._day, 0, 0, 0, 0, None)

    def __format__(self, fmt):
        if not fmt:
            return str(self)
        return self.strftime(fmt)

    def replace(self, year=None, month=None, day=None):
        return date(
            year if year is not None else self._year,
            month if month is not None else self._month,
            day if day is not None else self._day,
        )

    def _cmp_key(self):
        return (self._year, self._month, self._day)

    def __eq__(self, other):
        if isinstance(other, date) and not isinstance(other, datetime):
            return self._cmp_key() == other._cmp_key()
        if isinstance(other, date):
            return False
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, date):
            return self._cmp_key() < other._cmp_key()
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, date):
            return self._cmp_key() <= other._cmp_key()
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, date):
            return self._cmp_key() > other._cmp_key()
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, date):
            return self._cmp_key() >= other._cmp_key()
        return NotImplemented

    def __hash__(self):
        return hash(self._cmp_key())

    def __add__(self, other):
        if isinstance(other, timedelta):
            new_ord = self.toordinal() + other.days
            return date.fromordinal(new_ord)
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return self + timedelta(days=-other.days)
        if isinstance(other, date):
            return timedelta(days=self.toordinal() - other.toordinal())
        return NotImplemented


date.min = date(MINYEAR, 1, 1)
date.max = date(MAXYEAR, 12, 31)


# ----------- time ----------- #

class time:
    """Wall-clock time (without a date)."""

    __slots__ = ("_hour", "_minute", "_second", "_microsecond", "_tzinfo", "_fold")

    min = None
    max = None
    resolution = timedelta(microseconds=1)

    def __new__(cls, hour=0, minute=0, second=0, microsecond=0, tzinfo=None, *, fold=0):
        hour = _check_int(hour, "hour")
        minute = _check_int(minute, "minute")
        second = _check_int(second, "second")
        microsecond = _check_int(microsecond, "microsecond")
        if not (0 <= hour <= 23):
            raise ValueError("hour must be in 0..23")
        if not (0 <= minute <= 59):
            raise ValueError("minute must be in 0..59")
        if not (0 <= second <= 59):
            raise ValueError("second must be in 0..59")
        if not (0 <= microsecond <= 999_999):
            raise ValueError("microsecond must be in 0..999_999")
        if fold not in (0, 1):
            raise ValueError("fold must be 0 or 1")
        self = object.__new__(cls)
        self._hour = hour
        self._minute = minute
        self._second = second
        self._microsecond = microsecond
        self._tzinfo = tzinfo
        self._fold = fold
        return self

    @property
    def hour(self):
        return self._hour

    @property
    def minute(self):
        return self._minute

    @property
    def second(self):
        return self._second

    @property
    def microsecond(self):
        return self._microsecond

    @property
    def tzinfo(self):
        return self._tzinfo

    @property
    def fold(self):
        return self._fold

    def isoformat(self, timespec="auto"):
        return _format_time_iso(
            self._hour, self._minute, self._second, self._microsecond, timespec, self._tzinfo
        )

    __str__ = isoformat

    def __repr__(self):
        return f"datetime.time({self._hour}, {self._minute}, {self._second}, {self._microsecond})"

    def utcoffset(self):
        if self._tzinfo is None:
            return None
        return self._tzinfo.utcoffset(None)

    def tzname(self):
        if self._tzinfo is None:
            return None
        return self._tzinfo.tzname(None)

    def dst(self):
        if self._tzinfo is None:
            return None
        return self._tzinfo.dst(None)

    def strftime(self, fmt):
        return _strftime(
            fmt, 1900, 1, 1, self._hour, self._minute, self._second, self._microsecond, self._tzinfo
        )

    def replace(self, hour=None, minute=None, second=None, microsecond=None, tzinfo=True, *, fold=None):
        return time(
            hour if hour is not None else self._hour,
            minute if minute is not None else self._minute,
            second if second is not None else self._second,
            microsecond if microsecond is not None else self._microsecond,
            tzinfo if tzinfo is not True else self._tzinfo,
            fold=fold if fold is not None else self._fold,
        )

    def _cmp_key(self):
        return (self._hour, self._minute, self._second, self._microsecond)

    def __eq__(self, other):
        if isinstance(other, time):
            return self._cmp_key() == other._cmp_key()
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, time):
            return self._cmp_key() < other._cmp_key()
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, time):
            return self._cmp_key() <= other._cmp_key()
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, time):
            return self._cmp_key() > other._cmp_key()
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, time):
            return self._cmp_key() >= other._cmp_key()
        return NotImplemented

    def __hash__(self):
        return hash(self._cmp_key())


time.min = time(0, 0, 0)
time.max = time(23, 59, 59, 999_999)


# ----------- datetime ----------- #

class datetime(date):
    """Naive or aware datetime."""

    __slots__ = ("_hour", "_minute", "_second", "_microsecond", "_tzinfo", "_fold")

    min = None
    max = None
    resolution = timedelta(microseconds=1)

    def __new__(cls, year, month, day, hour=0, minute=0, second=0, microsecond=0,
                tzinfo=None, *, fold=0):
        self = date.__new__(cls, year, month, day)
        self._hour = _check_int(hour, "hour")
        self._minute = _check_int(minute, "minute")
        self._second = _check_int(second, "second")
        self._microsecond = _check_int(microsecond, "microsecond")
        self._tzinfo = tzinfo
        self._fold = fold
        return self

    @classmethod
    def now(cls, tz=None):
        if tz is None:
            y, mo, d, h, mi, s, us, _off = _impl.now_components()
            return cls(y, mo, d, h, mi, s, us)
        y, mo, d, h, mi, s, us, _off = _impl.utc_components()
        utc = cls(y, mo, d, h, mi, s, us, tzinfo=UTC)
        return utc.astimezone(tz)

    @classmethod
    def utcnow(cls):
        y, mo, d, h, mi, s, us, _off = _impl.utc_components()
        return cls(y, mo, d, h, mi, s, us)

    @classmethod
    def today(cls):
        return cls.now()

    @classmethod
    def fromtimestamp(cls, t, tz=None):
        if tz is None:
            y, mo, d, h, mi, s, us, _off = _impl.from_timestamp(t, False)
            return cls(y, mo, d, h, mi, s, us)
        y, mo, d, h, mi, s, us, _off = _impl.from_timestamp(t, True)
        return cls(y, mo, d, h, mi, s, us, tzinfo=UTC).astimezone(tz)

    @classmethod
    def utcfromtimestamp(cls, t):
        y, mo, d, h, mi, s, us, _off = _impl.from_timestamp(t, True)
        return cls(y, mo, d, h, mi, s, us)

    @classmethod
    def combine(cls, d, t, tzinfo=True):
        tz = t.tzinfo if tzinfo is True else tzinfo
        return cls(d.year, d.month, d.day, t.hour, t.minute, t.second, t.microsecond, tz, fold=t.fold)

    @classmethod
    def fromisoformat(cls, s):
        date_part = s[:10]
        rest = s[10:]
        d = date.fromisoformat(date_part)
        if not rest:
            return cls(d.year, d.month, d.day)
        if rest[0] in ("T", " "):
            rest = rest[1:]
        # Strip timezone first.
        tz = None
        for token, sign in ("+", 1), ("-", -1):
            idx = rest.rfind(token)
            if idx > 0:
                tz_part = rest[idx:]
                rest = rest[:idx]
                tz = _parse_tz(tz_part)
                break
        if rest.endswith("Z"):
            rest = rest[:-1]
            tz = UTC
        h, mi, s2, us = 0, 0, 0, 0
        bits = rest.split(":")
        if len(bits) >= 1 and bits[0]:
            h = int(bits[0])
        if len(bits) >= 2:
            mi = int(bits[1])
        if len(bits) >= 3:
            secpart = bits[2]
            if "." in secpart:
                a, b = secpart.split(".", 1)
                s2 = int(a)
                b = (b + "000000")[:6]
                us = int(b)
            else:
                s2 = int(secpart)
        return cls(d.year, d.month, d.day, h, mi, s2, us, tz)

    @property
    def hour(self):
        return self._hour

    @property
    def minute(self):
        return self._minute

    @property
    def second(self):
        return self._second

    @property
    def microsecond(self):
        return self._microsecond

    @property
    def tzinfo(self):
        return self._tzinfo

    @property
    def fold(self):
        return self._fold

    def date(self):
        return date(self._year, self._month, self._day)

    def time(self):
        return time(self._hour, self._minute, self._second, self._microsecond, fold=self._fold)

    def timetz(self):
        return time(self._hour, self._minute, self._second, self._microsecond,
                    self._tzinfo, fold=self._fold)

    def utcoffset(self):
        if self._tzinfo is None:
            return None
        return self._tzinfo.utcoffset(self)

    def tzname(self):
        if self._tzinfo is None:
            return None
        return self._tzinfo.tzname(self)

    def dst(self):
        if self._tzinfo is None:
            return None
        return self._tzinfo.dst(self)

    def timestamp(self):
        offset = 0
        if self._tzinfo is not None:
            off = self._tzinfo.utcoffset(self)
            if off is not None:
                offset = int(off.total_seconds())
        else:
            offset = _impl.local_utc_offset()
        return _impl.epoch_from_components(
            self._year, self._month, self._day, self._hour, self._minute,
            self._second, self._microsecond, offset
        )

    def astimezone(self, tz=None):
        if tz is None:
            tz = timezone(timedelta(seconds=_impl.local_utc_offset()))
        if self._tzinfo is None:
            # Treat as local.
            local_off = _impl.local_utc_offset()
            self = self.replace(tzinfo=timezone(timedelta(seconds=local_off)))
        target_off = tz.utcoffset(self)
        if target_off is None:
            raise ValueError("astimezone requires a known utcoffset")
        cur_off = self._tzinfo.utcoffset(self)
        delta = target_off - cur_off
        new = (self.replace(tzinfo=tz)) + delta
        return new

    def isoformat(self, sep="T", timespec="auto"):
        base = f"{self._year:04d}-{self._month:02d}-{self._day:02d}{sep}"
        base += _format_time_iso(
            self._hour, self._minute, self._second, self._microsecond, timespec, self._tzinfo
        )
        return base

    def __str__(self):
        return self.isoformat(sep=" ")

    def __repr__(self):
        return f"datetime.datetime({self._year}, {self._month}, {self._day}, {self._hour}, {self._minute}, {self._second}, {self._microsecond})"

    def ctime(self):
        return _strftime("%a %b %e %H:%M:%S %Y", self._year, self._month, self._day,
                         self._hour, self._minute, self._second, self._microsecond, self._tzinfo)

    def strftime(self, fmt):
        return _strftime(fmt, self._year, self._month, self._day,
                         self._hour, self._minute, self._second, self._microsecond, self._tzinfo)

    def replace(self, year=None, month=None, day=None, hour=None, minute=None,
                second=None, microsecond=None, tzinfo=True, *, fold=None):
        return datetime(
            year if year is not None else self._year,
            month if month is not None else self._month,
            day if day is not None else self._day,
            hour if hour is not None else self._hour,
            minute if minute is not None else self._minute,
            second if second is not None else self._second,
            microsecond if microsecond is not None else self._microsecond,
            tzinfo if tzinfo is not True else self._tzinfo,
            fold=fold if fold is not None else self._fold,
        )

    def _cmp_key(self):
        return (self._year, self._month, self._day, self._hour, self._minute,
                self._second, self._microsecond)

    def __eq__(self, other):
        if isinstance(other, datetime):
            return self._cmp_key() == other._cmp_key() and self._tzinfo == other._tzinfo
        return NotImplemented

    def __lt__(self, other):
        if isinstance(other, datetime):
            return self._cmp_key() < other._cmp_key()
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, datetime):
            return self._cmp_key() <= other._cmp_key()
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, datetime):
            return self._cmp_key() > other._cmp_key()
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, datetime):
            return self._cmp_key() >= other._cmp_key()
        return NotImplemented

    def __hash__(self):
        return hash((self._cmp_key(), self._tzinfo))

    def __add__(self, other):
        if isinstance(other, timedelta):
            total_us = (
                ((self.toordinal() + other.days) * 86_400
                 + self._hour * 3600 + self._minute * 60 + self._second
                 + other.seconds) * 1_000_000
                + self._microsecond + other.microseconds
            )
            extra_days, total_us = _divmod(total_us, 86_400 * 1_000_000)
            sec_us, us = _divmod(total_us, 1_000_000)
            new_ord = extra_days
            h, sec_us = _divmod(sec_us, 3600)
            mi, s = _divmod(sec_us, 60)
            y, mo, d = _impl.ordinal_to_components(new_ord)
            return datetime(y, mo, d, h, mi, s, us, self._tzinfo)
        return NotImplemented

    __radd__ = __add__

    def __sub__(self, other):
        if isinstance(other, timedelta):
            return self + timedelta(days=-other.days, seconds=-other.seconds,
                                    microseconds=-other.microseconds)
        if isinstance(other, datetime):
            us_a = ((self.toordinal() * 86_400 + self._hour * 3600 + self._minute * 60
                     + self._second) * 1_000_000 + self._microsecond)
            us_b = ((other.toordinal() * 86_400 + other._hour * 3600 + other._minute * 60
                     + other._second) * 1_000_000 + other._microsecond)
            return timedelta(microseconds=us_a - us_b)
        return NotImplemented


datetime.min = datetime(MINYEAR, 1, 1)
datetime.max = datetime(MAXYEAR, 12, 31, 23, 59, 59, 999_999)


# ----------- formatting helpers ----------- #

def _format_time_iso(h, m, s, us, timespec, tz):
    if timespec == "auto":
        timespec = "microseconds" if us else "seconds"
    if timespec == "hours":
        base = f"{h:02d}"
    elif timespec == "minutes":
        base = f"{h:02d}:{m:02d}"
    elif timespec == "seconds":
        base = f"{h:02d}:{m:02d}:{s:02d}"
    elif timespec == "milliseconds":
        base = f"{h:02d}:{m:02d}:{s:02d}.{us // 1000:03d}"
    elif timespec == "microseconds":
        base = f"{h:02d}:{m:02d}:{s:02d}.{us:06d}"
    else:
        raise ValueError(f"Unknown timespec value: {timespec!r}")
    if tz is None:
        return base
    off = tz.utcoffset(None)
    if off is None:
        return base
    total = int(off.total_seconds())
    sign = "+" if total >= 0 else "-"
    total = abs(total)
    hh, rem = _divmod(total, 3600)
    mm, ss = _divmod(rem, 60)
    if ss:
        base += f"{sign}{hh:02d}:{mm:02d}:{ss:02d}"
    else:
        base += f"{sign}{hh:02d}:{mm:02d}"
    return base


def _parse_tz(s):
    if not s:
        return None
    sign = 1 if s[0] == "+" else -1
    s = s[1:]
    hh, mm, ss = 0, 0, 0
    parts = s.split(":")
    hh = int(parts[0])
    if len(parts) > 1:
        mm = int(parts[1])
    if len(parts) > 2:
        if "." in parts[2]:
            ss = int(parts[2].split(".", 1)[0])
        else:
            ss = int(parts[2])
    total = sign * (hh * 3600 + mm * 60 + ss)
    return timezone(timedelta(seconds=total))


# Minimal strftime — handles the directives common Python code uses.
_MONTH_NAMES = ["", "January", "February", "March", "April", "May", "June",
                "July", "August", "September", "October", "November", "December"]
_MONTH_ABBR = ["", "Jan", "Feb", "Mar", "Apr", "May", "Jun",
               "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
_DAY_NAMES = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"]
_DAY_ABBR = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]


def _strftime(fmt, year, month, day, hour, minute, second, microsecond, tz):
    out = []
    i = 0
    n = len(fmt)
    wd = _impl.weekday(year, month, day)
    while i < n:
        c = fmt[i]
        if c != "%":
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= n:
            out.append("%")
            break
        directive = fmt[i]
        i += 1
        if directive == "Y":
            out.append(f"{year:04d}")
        elif directive == "y":
            out.append(f"{year % 100:02d}")
        elif directive == "m":
            out.append(f"{month:02d}")
        elif directive == "d":
            out.append(f"{day:02d}")
        elif directive == "e":
            out.append(f"{day:2d}")
        elif directive == "H":
            out.append(f"{hour:02d}")
        elif directive == "I":
            hh = hour % 12
            if hh == 0:
                hh = 12
            out.append(f"{hh:02d}")
        elif directive == "M":
            out.append(f"{minute:02d}")
        elif directive == "S":
            out.append(f"{second:02d}")
        elif directive == "f":
            out.append(f"{microsecond:06d}")
        elif directive == "p":
            out.append("AM" if hour < 12 else "PM")
        elif directive == "a":
            out.append(_DAY_ABBR[wd])
        elif directive == "A":
            out.append(_DAY_NAMES[wd])
        elif directive == "b" or directive == "h":
            out.append(_MONTH_ABBR[month])
        elif directive == "B":
            out.append(_MONTH_NAMES[month])
        elif directive == "j":
            # Day of year.
            ord_jan1 = _impl.days_to_ordinal(year, 1, 1)
            ord_today = _impl.days_to_ordinal(year, month, day)
            out.append(f"{ord_today - ord_jan1 + 1:03d}")
        elif directive == "w":
            # 0=Sunday..6=Saturday.
            out.append(str((wd + 1) % 7))
        elif directive == "u":
            out.append(str(wd + 1))
        elif directive == "z":
            if tz is None:
                out.append("")
            else:
                off = tz.utcoffset(None)
                if off is None:
                    out.append("")
                else:
                    total = int(off.total_seconds())
                    sign = "+" if total >= 0 else "-"
                    total = abs(total)
                    hh, rem = _divmod(total, 3600)
                    mm, _ = _divmod(rem, 60)
                    out.append(f"{sign}{hh:02d}{mm:02d}")
        elif directive == "Z":
            out.append(tz.tzname(None) if tz is not None else "")
        elif directive == "%":
            out.append("%")
        else:
            out.append("%")
            out.append(directive)
    return "".join(out)
