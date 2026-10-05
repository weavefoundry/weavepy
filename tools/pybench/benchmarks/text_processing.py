"""Everyday string work: split/join, strip, case, formatting, f-strings,
str.format, %, counting words, and textwrap."""

import collections
import textwrap

WORK = 300

PARA = (
    "The quick brown fox jumps over the lazy dog. Pack my box with five dozen "
    "liquor jugs! How vexingly quick daft zebras jump; the five boxing wizards "
    "jump quickly. Sphinx of black quartz, judge my vow. "
) * 6


def bench(n):
    total = 0
    for i in range(n):
        words = [w.strip(".,;!").lower() for w in PARA.split()]
        counts = collections.Counter(words)
        top = counts.most_common(5)
        total += sum(c for _, c in top)
        title = " ".join(w.capitalize() for w in words[:20])
        total += len(title.replace("Quick", "Slow"))
        rows = ["%-10s|%5d|%8.3f" % (w, len(w), len(w) / 3) for w in words[:30]]
        total += len("\n".join(rows))
        total += len(f"{i:>8}|{title[:10]!r}|{len(words):08.2f}")
        total += len("{0}-{1}-{name}".format(i, len(PARA), name=words[3]))
        total += len(textwrap.wrap(PARA, 60))
        total += PARA.count("the") + PARA.find("zebras") + int(PARA.startswith("The"))
        total += len("".join(reversed(words[5]))) + len(PARA.encode("utf-8"))
    return total
