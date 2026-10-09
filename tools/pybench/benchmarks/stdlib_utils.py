"""Utility-module code paths: pathlib and os.path manipulation,
statistics, datetime arithmetic and formatting, and urllib.parse."""

import datetime
import os.path
import pathlib
import statistics
import urllib.parse

WORK = 1500


def bench(n):
    total = 0
    base = pathlib.PurePosixPath("/srv/app/data")
    start = datetime.datetime(2026, 1, 1, 8, 30)
    for i in range(n):
        p = base / ("dir%d" % (i % 10)) / ("file%d.tar.gz" % i)
        total += len(p.name) + len(p.suffixes) + len(p.parent.parts) + len(p.stem)
        total += len(os.path.join("/a", "b%d" % i, "c.txt")) + len(os.path.splitext(str(p))[1])
        total += len(os.path.normpath("/a/./b/../c/%d" % i))
        d = start + datetime.timedelta(hours=i, minutes=i % 60)
        total += d.weekday() + len(d.isoformat()) + len(d.strftime("%Y-%m-%d %H:%M"))
        u = urllib.parse.urlsplit("https://example.com:8080/p/%d?q=%d&r=x#frag" % (i, i))
        total += len(u.path) + len(urllib.parse.parse_qs(u.query)) + (u.port or 0) % 7
        total += len(urllib.parse.quote("a b/c?%d" % i))
        if i % 100 == 0:
            data = [((j * 7919) % 1000) / 10 for j in range(200)]
            total += int(statistics.mean(data) + statistics.median(data) + statistics.pstdev(data))
    return total
