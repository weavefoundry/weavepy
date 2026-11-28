"""`del xs[a:b]` of a list.

A unit step's run is removed by its bounds alone (negative, past either
end, empty, or reversed); an extended slice, a list subclass and items
whose release runs code take the full path, and finalizers run after
the list has changed, last item first, as in CPython. Run it past the compile threshold.
"""


class Noisy:
    log = []

    def __init__(self, k):
        self.k = k

    def __del__(self):
        Noisy.log.append(self.k)


class L(list):
    pass


def run(n):
    for i in range(n):
        xs = list(range(10))
        del xs[-3:]
        assert xs == list(range(7))
        del xs[2:4]
        assert xs == [0, 1, 4, 5, 6]
        del xs[10:20]
        del xs[3:1]
        del xs[-20:1]
        assert xs == [1, 4, 5, 6]
        del xs[:]
        assert xs == []
        ys = list(range(8))
        del ys[::2]
        assert ys == [1, 3, 5, 7]
        del ys[1:3:1]
        assert ys == [1, 7]
        zs = L(range(5))
        del zs[1:3]
        assert zs == [0, 3, 4] and type(zs) is L
        ws = ["a", "b", (1, 2), None, 2.5]
        del ws[1:4]
        assert ws == ["a", 2.5]
    holder = [Noisy(1), Noisy(2), Noisy(3)]
    del holder[0:2]
    assert Noisy.log == [2, 1] and len(holder) == 1
    Noisy.log.clear()
    spaced = [Noisy(4), Noisy(5), Noisy(6), Noisy(7)]
    del spaced[::2]
    assert Noisy.log == [4, 6] and len(spaced) == 2


run(3000)
print("ok")
