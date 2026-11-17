"""Methods, `len()` and `in` on instances of builtin subclasses.

A dict, list, set or str subclass that doesn't override a method runs
the base's own on its native payload (`Counter.get`, a list subclass's
`append`, `len(od)`, `key in counter`). An override, including one
assigned after the call sites have warmed up, still runs; views keep the
instance; errors match the base's. Run it past the compile threshold.
"""

import collections


class D(dict):
    pass


class L(list):
    pass


class S(set):
    pass


class T(str):
    pass


class Tup(tuple):
    pass


class Late(dict):
    pass


class Own(list):
    def __len__(self):
        return 99

    def __contains__(self, item):
        return item == "own"

    def append(self, item):
        list.append(self, item * 2)


def run(n):
    total = 0
    for i in range(n):
        d = D(a=1)
        assert d.get("a") == 1 and d.get("z") is None and d.get("z", 5) == 5
        assert "a" in d and "z" not in d and len(d) == 1
        items = d.items()
        d["b"] = 2
        assert list(items) == [("a", 1), ("b", 2)] and len(items) == 2
        assert list(d.keys()) == ["a", "b"] and list(d.values()) == [1, 2]
        assert type(items.mapping) is type({}.items().mapping)
        assert items.mapping["b"] == 2
        try:
            d.get([])
        except TypeError:
            pass
        else:
            raise AssertionError("unhashable key")

        lst = L([1, 2])
        lst.append(3)
        assert lst.pop() == 3 and lst == [1, 2] and len(lst) == 2 and 2 in lst
        lst.extend([4])
        lst.insert(0, 0)
        lst.reverse()
        assert lst == [4, 2, 1, 0]
        c = lst.copy()
        assert type(c) is list and c == [4, 2, 1, 0]
        try:
            L().pop()
        except IndexError:
            pass
        else:
            raise AssertionError("pop from empty")

        s = S({1})
        s.add(2)
        s.discard(1)
        assert s == {2} and len(s) == 1 and 2 in s
        try:
            s.remove(7)
        except KeyError:
            pass
        else:
            raise AssertionError("remove missing")

        t = T(" Ab ")
        st = t.strip()
        assert st == "Ab" and type(st) is str
        assert t.lower() == " ab " and type(t.lower()) is str
        assert t.startswith(" A") and t.find("b") == 2 and len(t) == 4 and "Ab" in t
        assert T("-").join(["a", "b"]) == "a-b" and T("a b").split() == ["a", "b"]
        assert len(Tup((1, 2))) == 2 and 2 in Tup((1, 2))

        o = Own([1])
        o.append(5)
        assert list.__len__(o) == 2 and o[1] == 10
        assert len(o) == 99 and "own" in o and 1 not in o

        cnt = collections.Counter("abca")
        assert cnt.get("a") == 2 and "b" in cnt and len(cnt) == 3
        od = collections.OrderedDict(x=1)
        assert od.get("x") == 1 and "x" in od and len(od) == 1

        late = Late(k=1)
        if i == n // 2:
            Late.get = lambda self, k, default=None: "late"
            Late.__len__ = lambda self: 7
            Late.__contains__ = lambda self, k: True
        if i >= n // 2:
            assert late.get("k") == "late" and len(late) == 7 and "q" in late
        else:
            assert late.get("k") == 1 and len(late) == 1 and "q" not in late
        total += len(d) + len(lst)
    return total


assert run(3000) == 6 * 3000
print("ok")
