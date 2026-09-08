"""`datetime` construction, arithmetic, and formatting (RFC 0077 WS6).

The accelerator census fixture for `_datetime`: CPython runs these on
the C module; WeavePy on `_pydatetime`. The shape is the one the
Django and celery ecosystem probes time (`now()`-style construction,
`timedelta` arithmetic, comparison, `isoformat`/`strftime`, and
`fromisoformat` parsing).
"""

import os
from datetime import date, datetime, timedelta, timezone


def bench(n):
    total = 0
    base = datetime(2024, 1, 15, 8, 30, 0, tzinfo=timezone.utc)
    step = timedelta(minutes=37, seconds=11)
    day = timedelta(days=1)
    current = base
    for i in range(n):
        current = current + step
        if current - base > day * 30:
            base = base + day
        if current.hour == 12:
            total += 1
        total += current.weekday()
        if i & 15 == 0:
            s = current.isoformat()
            back = datetime.fromisoformat(s)
            total += back.minute
            total += len(current.strftime("%Y-%m-%d %H:%M:%S"))
        if i & 63 == 0:
            d = date(current.year, current.month, current.day)
            total += d.toordinal() & 7
            total += (d.replace(day=1) - date(2000, 1, 1)).days & 3
    return total


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "60000"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
