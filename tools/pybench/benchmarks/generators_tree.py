"""Walk a binary tree with recursive `yield from` generators and genexps."""

WORK = 15


class Tree:
    __slots__ = ("left", "value", "right")

    def __init__(self, left, value, right):
        self.left = left
        self.value = value
        self.right = right

    def __iter__(self):
        if self.left:
            yield from self.left
        yield self.value
        if self.right:
            yield from self.right


def build(lo, hi):
    if lo >= hi:
        return None
    mid = (lo + hi) // 2
    return Tree(build(lo, mid), mid, build(mid + 1, hi))


def pairs(it):
    it = iter(it)
    for a in it:
        b = next(it, 0)
        yield a, b


def bench(n):
    tree = build(0, 1 << n)
    total = 0
    for _ in range(3):
        total += sum(tree)
        total += sum(a * b for a, b in pairs(tree) if a & 1 == 0)
        total += max(x for x in tree if x % 7 == 3)
    return total
