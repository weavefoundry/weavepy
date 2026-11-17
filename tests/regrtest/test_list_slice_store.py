"""Slice assignment to a list in compiled code.

A unit-step slice of an exact list assigned a list's or a tuple's items
splices in place; bounds clamp as Python's do, a reversed slice inserts,
a list assigned to itself copies first, and an extended slice takes the
full path. A displaced item's finalizer runs before the next statement.
Run it past the compile threshold.
"""


class D:
    log = []

    def __init__(self, n):
        self.n = n

    def __del__(self):
        D.log.append(self.n)


def run(n):
    for _ in range(n):
        xs = [1, 2, 3, 4, 5]
        xs[:] = xs[2:]
        assert xs == [3, 4, 5]
        xs[1:2] = (7, 8, 9)
        assert xs == [3, 7, 8, 9, 5]
        xs[3:1] = [0]
        assert xs == [3, 7, 8, 0, 9, 5]
        xs[-2:] = []
        assert xs == [3, 7, 8, 0]
        xs[10:] = [1]
        assert xs == [3, 7, 8, 0, 1]
        xs[:] = xs
        assert xs == [3, 7, 8, 0, 1]
        xs[::2] = [0, 0, 0]
        assert xs == [0, 7, 0, 0, 0]
        try:
            xs[::2] = [1]
        except ValueError:
            pass
        else:
            raise AssertionError("extended slice of the wrong length")
        ys = [D(1), D(2)]
        ys[:1] = [3]
        assert D.log == [1], D.log
        D.log.clear()
        del ys
        assert D.log == [2]
        D.log.clear()


run(3000)
print("ok")
