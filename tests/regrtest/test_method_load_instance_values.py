"""A method call of a value an instance holds itself (`self.fn(x)` with
`fn` set per instance), which compiled code remembers by the instance's
layout, keeps CPython's results: the instance's own value, a method of the
same name added to the class later (the instance's still wins), a
property added later (which wins), the value deleted (the class's method
answers), values of different instances, and a class with its own
`__getattribute__`."""


class Node:
    def __init__(self, fn):
        self.fn = fn

    def run(self, x):
        return self.fn(x)


class Logged:
    seen = 0

    def __init__(self, fn):
        self.fn = fn

    def __getattribute__(self, name):
        Logged.seen += 1
        return object.__getattribute__(self, name)

    def run(self, x):
        return self.fn(x)


def drive(nodes, x):
    return [n.run(x) for n in nodes]


nodes = [Node(lambda v: v + 1), Node(lambda v: v * 2), Node(str)]
for i in range(2000):
    assert drive(nodes, i) == [i + 1, 2 * i, str(i)]
# A method of the same name on the class: the instance's value wins.
Node.fn = lambda self, v: "class"
assert drive(nodes, 3) == [4, 6, "3"]
# The value deleted: the class's method answers.
del nodes[0].fn
assert drive(nodes, 3) == ["class", 6, "3"]
# A property of the name: a data descriptor wins over the instance.
Node.fn = property(lambda self: (lambda v: "prop"))
assert drive(nodes, 3) == ["prop", "prop", "prop"]
del Node.fn
logged = [Logged(abs)]
before = Logged.seen
for i in range(2000):
    assert drive(logged, -i) == [i]
assert Logged.seen - before >= 4000, Logged.seen - before
print("ok")
