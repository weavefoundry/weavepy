"""List, dict, and set comprehensions over records, with filtering and
nested loops (after pyperformance's `comprehensions`)."""

from dataclasses import dataclass
from enum import Enum

WORK = 6000


class Kind(Enum):
    A = 1
    B = 2
    C = 3


@dataclass
class Widget:
    widget_id: int
    creator_id: int
    kind: Kind
    tags: list
    has_frame: bool


def make(n):
    kinds = [Kind.A, Kind.B, Kind.C]
    return [Widget(i, i % 17, kinds[i % 3], ["t%d" % (i % 5), "u%d" % (i % 7)], i % 4 == 0)
            for i in range(n)]


def summarize(widgets):
    by_creator = {}
    for w in widgets:
        by_creator.setdefault(w.creator_id, []).append(w)
    framed = [w.widget_id for w in widgets if w.has_frame and w.kind is not Kind.C]
    tags = {t for w in widgets for t in w.tags}
    counts = {k: len(v) for k, v in by_creator.items()}
    pairs = [(a.widget_id, b.widget_id) for a in widgets[:40] for b in widgets[:40]
             if a.creator_id == b.creator_id and a.widget_id < b.widget_id]
    kinds = {k.name: sum(1 for w in widgets if w.kind is k) for k in Kind}
    return len(framed) + len(tags) + sum(counts.values()) + len(pairs) + kinds["B"]


def bench(n):
    total = 0
    for _ in range(10):
        total += summarize(make(n))
    return total
