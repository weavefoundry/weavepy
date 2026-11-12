"""Compiled code's `cls.__new__` reads keep CPython's results: the builtin
constructor a plain class inherits, a class's own `__new__` (a Python
function, which receives the class explicitly), a built-in subclass's
inherited constructor, several classes at one site, and a class whose
`__new__` is assigned after the site has warmed up."""


class Plain:
    pass


class Own:
    def __new__(cls, *args):
        inst = super().__new__(cls)
        inst.made = True
        return inst


class Num(int):
    pass


class Text(str):
    pass


def make(cls):
    return cls.__new__(cls)


for _ in range(3000):
    assert type(make(Plain)) is Plain
assert make(Own).made and type(make(Own)) is Own
assert make(Num) == 0 and type(make(Num)) is Num
assert make(Text) == "" and type(make(Text)) is Text
assert Plain.__new__ is object.__new__ and Num.__new__ is int.__new__

kinds = [Plain, Own, Num, Text]
for _ in range(3000):
    for cls in kinds:
        assert type(make(cls)) is cls

Plain.__new__ = staticmethod(lambda cls: "replaced")
assert make(Plain) == "replaced"
del Plain.__new__
assert type(make(Plain)) is Plain
try:
    make(int)
except TypeError:
    raise AssertionError("int.__new__(int) failed")
print("ok")
