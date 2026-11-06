"""f-strings (and `%` formats the compiler turns into them) from compiled
code keep CPython's results: `!s` and `!r` of strings, ints, floats,
bools and `None`, spec-less formatting, joins of up to eight parts and
more, and the full protocol for everything else (a `__str__`,
`__repr__` or `__format__` of a class, a spec, an int subclass), with no
reference left over."""
import sys


class Loud:
    def __str__(self):
        return "loud"

    def __repr__(self):
        return "<Loud>"

    def __format__(self, spec):
        return "fmt[" + spec + "]"


class Num(int):
    def __str__(self):
        return "num"

    def __format__(self, spec):
        return "N" + spec


word = "w" * 40
big = 1 << 80


def check(i):
    s, f, b = "ab'c", 2.5, i % 2 == 0
    assert f"{s}|{s!r}|{i}|{i!r}|{f}|{f!r}" == "ab'c|" + repr(s) + "|%d|%d|2.5|2.5" % (i, i)
    assert f"{b}-{b!r}-{None}-{None!r}" == "%s-%s-None-None" % (b, b)
    assert "%s:%s" % (word, i) == word + ":" + str(i)
    assert f"{big}" == str(big) and f"{-big!r}" == repr(-big)
    assert f"{Loud()}|{Loud()!s}|{Loud()!r}|{Loud():x}" == "fmt[]|loud|<Loud>|fmt[x]"
    assert f"{Num(3)}|{Num(3)!s}|{Num(3):>2}" == "N|num|N>2"
    assert f"{i:>4}|{f:.2f}|{s:^7}" == "%4d|%.2f| ab'c  " % (i, f)
    assert f"{1}{2}{3}{4}{5}{6}{7}{8}{9}{10}" == "12345678910"
    assert f"{[i]}{(i,)}{ {i: i} }" == "[%d](%d,){%d: %d}" % (i, i, i, i)
    t = f"{word}{i}"
    assert t is not word and t.startswith(word)


before = sys.getrefcount(word)
# Past the frame JIT's threshold, so later rounds run compiled code.
for i in range(3000):
    check(i)
assert sys.getrefcount(word) == before
print("ok")
