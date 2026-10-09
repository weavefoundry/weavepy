"""Common regular-expression workloads: search, findall, sub with a
callback, split, named groups, and compile of a few patterns."""

import re

WORK = 60

TEXT = "\n".join(
    "2026-10-%02d 12:%02d:%02d INFO user%d@example.com GET /api/v1/items/%d?q=a+b status=200 time=%dms"
    % (i % 28 + 1, i % 60, (i * 7) % 60, i, i * 13, i % 400)
    for i in range(200)
)

LINE = re.compile(r"^(?P<date>\d{4}-\d{2}-\d{2}) (?P<time>[\d:]+) (?P<level>\w+) (?P<email>\S+) (?P<verb>[A-Z]+) (?P<path>\S+)", re.M)
EMAIL = re.compile(r"[\w.+-]+@[\w-]+\.[\w.]+")
NUM = re.compile(r"\d+")
WORD = re.compile(r"\b[a-z]{3,}\b", re.I)


def bench(n):
    total = 0
    for i in range(n):
        for m in LINE.finditer(TEXT):
            total += len(m.group("path")) + len(m["level"])
        total += len(EMAIL.findall(TEXT))
        total += len(NUM.sub(lambda m: str(int(m.group()) + 1), TEXT[:2000]))
        total += len(re.split(r"[\s=?]+", TEXT[:3000]))
        total += sum(1 for _ in WORD.finditer(TEXT[:4000]))
        if re.match(r"(\d+)-(\d+)-(\d+)", TEXT):
            total += 1
        total += len(re.sub(r"status=(\d+)", r"code:\1", TEXT[:1000]))
    return total
