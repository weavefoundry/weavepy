"""End-to-end fixture for RFC 0023 drop-in parity additions.

Touches the new builtins, the new frozen stdlib modules, the walrus
operator, PEP 420 namespace packages, PEP 695 type aliases / type
parameters, generic aliases, and the dict-view / mappingproxy /
SimpleNamespace object types.
"""

import bisect
import copy
import operator
import statistics
import textwrap
import stat
import posixpath
import numbers
import sys


print("=== builtins ===")
print(pow(2, 10))
print(pow(3, 4, 5))
print(memoryview(b"weave").tobytes())

print("=== walrus ===")
if (n := 7) > 5:
    print("n =", n)
total = 0
i = 0
while (x := i * 2) < 10:
    total += x
    i += 1
print("walrus loop sum:", total)

print("=== generic alias ===")
ints: list[int] = [1, 2, 3]
print(ints, ints[0] + ints[1])
print(list[int].__origin__ is list)

print("=== bisect ===")
data = [1, 3, 5, 7, 9]
print(bisect.bisect_left(data, 5), bisect.bisect_right(data, 5))
bisect.insort(data, 6)
print(data)

print("=== copy ===")
xs = [[1, 2], [3, 4]]
ys = copy.deepcopy(xs)
ys[0].append(99)
print("orig:", xs)
print("deep:", ys)

print("=== operator ===")
print(operator.itemgetter(1)([10, 20, 30]))
print(operator.add(40, 2))

print("=== statistics ===")
print(statistics.mean([1, 2, 3, 4, 5]))
print(statistics.median([1, 2, 3, 4]))
print(round(statistics.stdev([2, 4, 4, 4, 5, 5, 7, 9]), 4))

print("=== textwrap ===")
print(textwrap.fill("the quick brown fox jumps over the lazy dog", width=15))

print("=== stat ===")
print(stat.S_ISREG(0o100644))
print(stat.S_IMODE(0o100755))

print("=== posixpath ===")
print(posixpath.join("/a", "b", "c.txt"))
print(posixpath.splitext("foo/bar.tar.gz"))

print("=== numbers ===")
print(isinstance(3, numbers.Number))
print(isinstance(3, numbers.Integral))
print(isinstance(3.14, numbers.Real))

print("=== dict views ===")
d = {"a": 1, "b": 2}
print(sorted(d.keys()))
print(sorted(d.values()))
print(sorted(d.items()))

print("=== sys.implementation ===")
print(sys.implementation.name)
print(sys.implementation.version[0])


def show_type_alias():
    # PEP 695 — parameterised type aliases. Body lazily evaluated.
    type Pair[T] = tuple[T, T]
    print("alias name:", type(Pair).__name__)

show_type_alias()

print("ok")
