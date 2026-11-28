"""Property reads at a compiled site that sees several classes.

`email`'s token tree reads `x.value` and `x.all_defects` over two node
classes, each with a property of its own; one getter runs as an
activation (it joins a generator), the other is a leaf. The site keeps
each class's getter. A class whose property changes, a subclass that
replaces the property with a plain attribute, and a getter that raises
keep CPython's behavior. Items are heap objects. Run it past the compile
threshold.
"""


class Leaf(str):
    def __new__(cls, value):
        self = super().__new__(cls, value)
        self.defects = []
        return self

    @property
    def value(self):
        return str(self)

    @property
    def all_defects(self):
        return list(self.defects)


class Node(list):
    def __init__(self, *args):
        super().__init__(*args)
        self.defects = [object()]

    @property
    def value(self):
        return "".join(x.value for x in self if x.value)

    @property
    def all_defects(self):
        return sum((x.all_defects for x in self), self.defects)


class Plain(Leaf):
    value = "plain"


class Raises(Leaf):
    @property
    def value(self):
        raise AttributeError("no value")


def values(xs):
    return [x.value for x in xs]


def tree():
    return Node([Node([Leaf("a"), Leaf("b")]), Leaf("c"), Node([Node([Leaf("d")])])])


class Swap:
    @property
    def value(self):
        return 1


for i in range(3000):
    t = tree()
    assert t.value == "abcd"
    assert len(t.all_defects) == 4
    assert values([Leaf("x"), Node([Leaf("y")]), Plain("z")]) == ["x", "y", "plain"]
    try:
        values([Leaf("x"), Raises("r")])
    except AttributeError as e:
        assert str(e) == "no value"
    else:
        raise AssertionError("no raise")
    s = Swap()
    assert values([s, Leaf("q")]) == [i % 2 + 1 if i > 1500 else 1, "q"]
    if i == 1500:
        Swap.value = property(lambda self: 2)
    if i == 1501:
        Swap.value = property(lambda self: 1)
    if i > 1501:
        Swap.value = property(lambda self, i=i: (i + 1) % 2 + 1)
