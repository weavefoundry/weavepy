"""An instance with a Python `__hash__` as a dict key and one with a Python
`__bool__` in a truth test, from compiled code: the hash evaluated in place
finds the key itself (an equal one through `__eq__`), `-1` hashes as `-2`,
and a `__bool__` returning a non-bool raises TypeError."""
class H:
    def __init__(self, v): self.v = v
    def __hash__(self): return self.v
    def __eq__(self, o): return isinstance(o, H) and o.v == self.v
class Neg:
    def __hash__(self): return -1
class B:
    def __init__(self, x): self.x = x
    def __bool__(self): return self.x
class Bad:
    def __bool__(self): return 1
a, b2, c = H(3), H(3), H(4)
d = {a: "a", c: "c", 3: "int"}
n = Neg()
dn = {n: 9}
def run(k):
    t = []
    for i in range(k):
        t.append((d[a], d[c], d[b2], d.get(H(99), "miss"), dn[n], hash(n), bool(B(True)), bool(B(False)), 1 if B(i % 2 == 1) else 0))
        try:
            if Bad(): pass
        except TypeError as e:
            assert "should return bool" in str(e), e
        else:
            raise AssertionError("Bad.__bool__ didn't raise")
    return t[-1]
assert run(4000) == ("a", "c", "a", "miss", 9, -2, True, False, 1)
print("ok")
