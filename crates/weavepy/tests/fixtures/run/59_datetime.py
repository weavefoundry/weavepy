from datetime import date, time, datetime, timedelta, timezone, UTC


d = date(2024, 3, 15)
print(d.year, d.month, d.day)
print(d.isoformat())
print(d.weekday())
print(d.toordinal() > 0)
print(date.fromisoformat("2024-12-25"))


t = time(15, 30, 45)
print(t.isoformat())


dt = datetime(2024, 3, 15, 9, 0, 0)
print(dt.isoformat())
print(dt.year, dt.month, dt.day, dt.hour)
print(dt.date())
print(dt.time())


# Arithmetic.
d2 = d + timedelta(days=5)
print(d2)
print((d2 - d).days)
print(dt + timedelta(hours=3))


# Timezone.
utc_dt = datetime(2024, 1, 1, tzinfo=UTC)
print(utc_dt.tzinfo)
print(utc_dt.utcoffset())


# fromisoformat with time.
dt2 = datetime.fromisoformat("2024-06-01T12:00:00")
print(dt2)
print(datetime.fromisoformat("2024-06-01T12:00:00.123456"))


# Comparison and equality.
print(date(2024, 1, 1) < date(2024, 1, 2))
print(date(2024, 1, 1) == date(2024, 1, 1))
print(time(10, 0) < time(11, 0))


# timedelta arithmetic.
td = timedelta(seconds=3600)
print(td.total_seconds())
print(timedelta(days=1) + timedelta(hours=12))
