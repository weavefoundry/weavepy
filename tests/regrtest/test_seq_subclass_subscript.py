"""Subscripts of `list` and `tuple` subclasses' instances.

`email`'s `TokenList` (a `list` subclass) reads `self[0]` in its
properties. Compiled code and the core loop index the payload in place
when the class keeps the builtin `__getitem__`; a class that overrides
it, a slice, an out-of-range index and a non-int key keep the full
handler's behavior. Items are heap objects. Run it past the compile
threshold.
"""


class Item:
    pass


class L(list):
    def first(self):
        return self[0]


class T(tuple):
    pass


class Over(list):
    def __getitem__(self, i):
        return ("over", i)


def read(seq, i):
    return seq[i]


a, b = Item(), Item()
lst, tup, over = L([a, b]), T((a, b)), Over([a])
for _ in range(3000):
    assert read(lst, 0) is a and read(lst, -1) is b and lst.first() is a
    assert read(tup, 1) is b and read(tup, -2) is a
    assert read(over, 0) == ("over", 0)
    assert read(lst, slice(0, 1)) == [a]
    try:
        read(lst, 2)
    except IndexError:
        pass
    else:
        raise AssertionError("no IndexError")
    try:
        read(tup, "x")
    except TypeError:
        pass
    else:
        raise AssertionError("no TypeError")
