"""Deleting an instance attribute keeps the split layout (no dictionary
is materialized), and the instance still behaves exactly like a dict-backed
one: lookups, `__dict__` order, re-adding, AttributeError, prompt
finalization, and compiled code's cached field reads."""


class O:
    def __init__(self):
        self.a = 1
        self.b = 2
        self.c = 3
        self.d = 4

    def total(self):
        return self.a + self.c + self.d


class Fin:
    log = []

    def __del__(self):
        Fin.log.append(1)


def run():
    for _ in range(2000):
        o = O()
        del o.b
        assert not hasattr(o, "b")
        assert getattr(o, "b", None) is None
        assert o.a == 1 and o.c == 3 and o.d == 4
        assert o.total() == 8
        assert list(vars(o)) == ["a", "c", "d"], vars(o)
        o.b = 5
        assert list(vars(o).items()) == [("a", 1), ("c", 3), ("d", 4), ("b", 5)]
        assert o.b == 5

        p = O()
        del p.a, p.d
        assert p.__dict__ == {"b": 2, "c": 3}
        p.c = 7
        assert p.c == 7 and getattr(p, "a", None) is None
        try:
            del p.a
        except AttributeError as e:
            assert "'a'" in str(e), e
        else:
            raise AssertionError("deleted twice")

        # The last attribute simply leaves.
        q = O()
        del q.d
        q.e = 9
        assert list(q.__dict__) == ["a", "b", "c", "e"]
        assert not hasattr(q, "d")

        r = O()
        del r.a
        del r.b
        del r.c
        del r.d
        assert r.__dict__ == {}
        r.z = 1
        assert r.__dict__ == {"z": 1}

        # A fresh instance of the class keeps its fast layout.
        s = O()
        assert s.total() == 8 and list(vars(s)) == ["a", "b", "c", "d"]

    # The deleted value dies at the `del`.
    o = O()
    o.b = Fin()
    del o.b
    assert Fin.log == [1], Fin.log
    o.c = Fin()
    del o.a, o.c
    assert Fin.log == [1, 1], Fin.log
    assert vars(o) == {"d": 4}


run()
print("ok")
