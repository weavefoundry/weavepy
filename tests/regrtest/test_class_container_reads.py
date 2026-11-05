"""A class's dict, list or set read through an instance (`self.TABLE`),
which compiled code serves from its site's cache, keeps CPython's
results: the container's current contents, a rebound or deleted class
attribute, a subclass's own table, an instance attribute that shadows the
class's, and a property put in its place."""


class Parser:
    PREC = {"+": 1, "*": 2}
    ORDER = ["a", "b"]
    SEEN = {"x"}

    def prec(self, op):
        return self.PREC.get(op, 0)

    def first(self):
        return self.ORDER[0]

    def has(self, k):
        return k in self.SEEN


class Strict(Parser):
    PREC = {"+": 10}


def run(objs):
    t = 0
    for p in objs:
        t += p.prec("+") + p.prec("*") + len(p.first()) + p.has("x")
    return t


objs = [Parser(), Strict(), Parser()]
# Past the frame JIT's threshold, so later rounds run compiled code.
for _ in range(3000):
    assert run(objs) == (1 + 2 + 1 + 1) * 2 + (10 + 0 + 1 + 1)

# The contents change in place: the same table, read again.
Parser.PREC["*"] = 5
Parser.ORDER[0] = "abc"
Parser.SEEN.discard("x")
assert run(objs) == (1 + 5 + 3 + 0) * 2 + (10 + 0 + 3 + 0)

# A rebound class attribute, and a deleted subclass override.
Parser.PREC = {"+": 7}
assert run(objs) == (7 + 0 + 3 + 0) * 2 + (10 + 0 + 3 + 0)
del Strict.PREC
assert run(objs) == (7 + 0 + 3 + 0) * 3

# An instance's own attribute shadows the class's for it alone.
objs[0].PREC = {"+": 100}
assert run(objs) == (100 + 0 + 3) + (7 + 0 + 3) * 2

# A property replaces the table for every instance without its own value.
Parser.ORDER = property(lambda self: ["zz"])
assert [p.first() for p in objs] == ["zz"] * 3
print("ok")
