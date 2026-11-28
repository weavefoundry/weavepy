"""`hex`, `oct` and `bin` of ints, as CPython formats them.

Machine ints (zero, the extremes, negatives), big ints, bools and an
`__index__` object, in a loop past the compile threshold.
"""

import sys


class Idx:
    def __index__(self):
        return 10


def run(n):
    for i in range(n):
        assert hex(i) == "0x%x" % i and oct(-i) == ("-0o%o" % i if i else "0o0")
        assert bin(i & 7) == "0b" + format(i & 7, "b")


run(3000)
cases = {
    0: ("0x0", "0o0", "0b0"),
    255: ("0xff", "0o377", "0b11111111"),
    -255: ("-0xff", "-0o377", "-0b11111111"),
    sys.maxsize: ("0x7fffffffffffffff", "0o777777777777777777777", "0b" + "1" * 63),
    -sys.maxsize - 1: ("-0x8000000000000000", "-0o1000000000000000000000", "-0b1" + "0" * 63),
    2**64: ("0x10000000000000000", "0o2000000000000000000000", "0b1" + "0" * 64),
    True: ("0x1", "0o1", "0b1"),
}
for v, (h, o, b) in cases.items():
    assert (hex(v), oct(v), bin(v)) == (h, o, b), v
assert (hex(Idx()), oct(Idx()), bin(Idx())) == ("0xa", "0o12", "0b1010")
print("ok")
