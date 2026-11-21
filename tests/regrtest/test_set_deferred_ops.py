"""A set mutated from an element's `__eq__` while another operation holds it.

The mutation waits for the set's next in-place change, which applies it
first. It never reaches another set: a set that dies with a mutation
pending drops it, so a new set at the same address keeps everything added
to it.
"""

armed = False


class Bad:
    def __hash__(self):
        return 1

    def __eq__(self, other):
        if armed:
            victim.clear()
        return True


class K:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return self.v


def defer_clear():
    global armed, victim
    armed = False
    victim = {Bad(), Bad()}
    armed = True
    try:
        Bad() in victim
    finally:
        armed = False


for _ in range(200):
    defer_clear()
    k = K(7)
    victim.add(k)
    assert victim == {k}, victim

for _ in range(200):
    defer_clear()
    victim = None
    for j in range(50):
        f = set()
        f.add(K(j))
        f.add(K(-j - 1))
        assert len(f) == 2, f
print("ok")
