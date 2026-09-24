"""Class-key equality runs only with the attribute caller's frame published."""

import sys


def read(root, n):
    marker = "current"
    total = 0
    for i in range(n):
        total += root.value
        total += root.other
    return total


class Ordinary:
    value = 7
    other = 11


plain = Ordinary()
for _ in range(2):
    assert read(plain, 1200) == 21600


events = []
capture = False


class Key:
    def __hash__(self):
        return hash("__getattribute__")

    def __eq__(self, other):
        if capture:
            caller = sys._getframe(1)
            events.append((caller.f_code.co_name, caller.f_locals.get("i"),
                           caller.f_locals.get("marker")))
        return other == "__getattribute__"


Odd = type("Odd", (), {Key(): object.__getattribute__,
                       "value": 7, "other": 11})
odd = object.__new__(Odd)
capture = True
assert read(odd, 2) == 36
capture = False
assert all(name == "read" and i in (0, 1) and marker == "current"
           for name, i, marker in events), events
print("class-key callbacks: ok")
