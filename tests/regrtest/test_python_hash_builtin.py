"""`hash(x)` and `d[x]` from compiled code for an instance whose Python
`__hash__` evaluates as a pure leaf (a frozen dataclass hashing its fields,
one hashing another such instance): CPython's hashes and lookups, and a
`__hash__` that hashes itself still ends in RecursionError."""
import dataclasses, sys
@dataclasses.dataclass(frozen=True)
class FD:
    a: int
    b: str
class Self:
    def __hash__(self): return hash(self)
class Outer:
    def __init__(self, inner): self.inner = inner
    def __hash__(self): return hash(self.inner) + 1
fd = FD(1, "x"); o = Outer(FD(2, "y"))
def run(n):
    t = 0
    for i in range(n):
        t += (hash(fd) == hash((1, "x"))) + (hash(o) == hash((2, "y")) + 1) + ({fd: 1}[FD(1, "x")])
        assert fd in {fd} and FD(1, "x") in {fd: 0} and FD(3, "z") not in {fd} and o in {o}
    return t
assert run(5000) == 15000
try:
    hash(Self())
except RecursionError:
    pass
else:
    raise AssertionError("self-hashing __hash__ didn't raise")
print("ok")
